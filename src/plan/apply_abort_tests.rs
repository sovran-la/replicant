//! Review P1: an abort raised while a unit is being announced, or during
//! the confirm-delete note before its marker, stops that unit before any of
//! it is written: no marker, no rsync, its trees untouched, and the journal
//! says it stopped before it began.

use super::progress_tests::sync_watched;
use super::*;
use crate::daemon::local::HANGUP;
use crate::daemon::wire::{HomeReply, HomeRequest};
use crate::plan::HomeLink;
use crate::rsync::Abort;

/// The sync stopped at `rel` before it began: `Failed`, saying so.
fn stopped_before(r: &SyncReport, rel: &str) {
    match &r.outcome {
        Outcome::Failed { rel: at, why } => {
            assert_eq!(at, rel);
            assert!(why.contains(HANGUP), "{why}");
            assert!(
                why.ends_with("stopped before this unit began, nothing of it written"),
                "{why}"
            );
        }
        o => panic!("{o:?}"),
    }
    assert_eq!(r.outcome.exit_code(), 1);
}

/// Home's last journal line is the sync's, failed at `rel` before it began.
fn journaled(home: &Home, rel: &str) {
    let log = home.log(&Default::default()).unwrap();
    let last = log.last().unwrap();
    assert_eq!(last.event, "sync");
    assert!(
        last.detail.contains(&format!("failed at {rel}"))
            && last.detail.contains("stopped before this unit began"),
        "{}",
        last.detail
    );
    assert!(
        !log.iter()
            .any(|l| l.path == rel && l.detail.contains("interrupted")),
        "{log:#?}"
    );
}

#[test]
fn an_abort_raised_as_a_unit_is_announced_writes_nothing_of_it() {
    let bed = Bed::enrolled();
    tree(&bed.home_hd.join("work"));
    let home = bed.home();
    let home_beta = testutil::fingerprint(&bed.home_hd.join("work/beta"));
    // The CLI dies while beta's start is being written to it.
    let (r, events) = sync_watched(&bed, &home, &|e, abort| {
        if let ProgressEvent::UnitStarted { rel, .. } = e
            && rel == "work/beta"
        {
            abort.raise_because(HANGUP);
        }
    });
    stopped_before(&r, "work/beta");
    let rels: Vec<&str> = r.applied.iter().map(|a| a.rel.as_str()).collect();
    assert_eq!(rels, ["work", "work/alpha"]);
    let markers: Vec<Marker> = crate::marker::store(&bed.leaf_dirs.state()).all().unwrap();
    assert!(markers.is_empty(), "not even a marker: {markers:?}");
    assert!(!bed.leaf_hd.join("work/beta").exists(), "no rsync ran");
    assert!(!bed.leaf_hd.join("work/nested/gamma").exists());
    assert_eq!(
        testutil::fingerprint(&bed.home_hd.join("work/beta")),
        home_beta
    );
    assert!(
        crate::baseline::store(&bed.leaf_dirs.state())
            .get::<Baseline>("work/beta")
            .unwrap()
            .is_none()
    );
    journaled(&home, "work/beta");
    // Its start is answered by its end, which says it was stopped.
    let last: Vec<&ProgressEvent> = events.iter().rev().take(2).collect();
    assert!(
        matches!(last[1], ProgressEvent::UnitStarted { rel, .. } if rel == "work/beta"),
        "{events:#?}"
    );
    match last[0] {
        ProgressEvent::UnitDone {
            rel,
            outcome: UnitOutcome::Failed { why },
            files_changed: 0,
            deleted: 0,
            push: None,
            ..
        } => {
            assert_eq!(rel, "work/beta");
            assert!(why.contains("stopped before this unit began"), "{why}");
        }
        e => panic!("{e:?}"),
    }
    assert_eq!(events.len(), 6, "{events:#?}");
    assert!(home.queue.view().is_empty(), "the entry was given back");

    // Nothing to resume: the next sync pulls beta afresh.
    let r = yes(&bed, &home);
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    let beta = r.applied.iter().find(|a| a.rel == "work/beta").unwrap();
    assert!(!beta.resumed);
}

/// Home, through which this sync's abort is raised as `when` asks home.
struct RaiseOn<'a> {
    inner: InProcess<'a>,
    abort: Abort,
    when: fn(&HomeRequest) -> bool,
}

impl HomeLink for RaiseOn<'_> {
    fn call(&self, request: HomeRequest) -> crate::Result<HomeReply> {
        let raise = (self.when)(&request);
        let reply = self.inner.call(request);
        if raise {
            self.abort.raise_because(HANGUP);
        }
        reply
    }
}

/// `sync` as `leaf` confirming `confirm`, the abort raised once home
/// answers the confirm-delete note.
fn raised_on_the_note(bed: &Bed, home: &Home, confirm: &str) -> (SyncReport, Vec<ProgressEvent>) {
    let abort = Abort::default();
    let link = RaiseOn {
        inner: InProcess {
            home,
            host: "leaf".into(),
            home_dir: bed.leaf_hd.clone(),
        },
        abort: abort.clone(),
        when: |r| matches!(r, HomeRequest::Note { event, .. } if event == "confirm-delete"),
    };
    let cfg = bed.leaf_cfg();
    let dirs = (bed.leaf_dirs.state(), bed.leaf_dirs.scratch());
    let seen = std::sync::Mutex::new(Vec::new());
    let emit = |e: ProgressEvent| {
        if !matches!(e, ProgressEvent::UnitProgress { .. }) {
            seen.lock().unwrap().push(e)
        }
    };
    let progress = Progress::new(&emit, abort);
    let opts = SyncOpts {
        confirm_delete: vec![confirm.into()],
        ..Default::default()
    };
    let ctx = ctx(bed, &cfg, &dirs, crate::plan::RSYNC);
    let r = crate::plan::apply::sync_with(&link, ctx, &opts, &progress).unwrap();
    (r, seen.into_inner().unwrap())
}

#[test]
fn an_abort_during_a_deletion_note_writes_no_marker_and_deletes_nothing_here() {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/r"));
    let home = bed.home();
    assert_eq!(yes(&bed, &home).outcome, Outcome::Done);
    std::fs::remove_dir_all(bed.home_hd.join("work/r")).unwrap();
    let before = testutil::fingerprint(&bed.leaf_hd);
    let (r, events) = raised_on_the_note(&bed, &home, "work/r");
    stopped_before(&r, "work/r");
    assert!(r.applied.is_empty());
    assert_eq!(
        testutil::fingerprint(&bed.leaf_hd),
        before,
        "delete here ran"
    );
    let markers: Vec<Marker> = crate::marker::store(&bed.leaf_dirs.state()).all().unwrap();
    assert!(markers.is_empty(), "not even a marker: {markers:?}");
    assert!(
        crate::baseline::store(&bed.leaf_dirs.state())
            .get::<Baseline>("work/r")
            .unwrap()
            .is_some(),
        "the baseline stands"
    );
    journaled(&home, "work/r");
    assert_eq!(events.len(), 2, "{events:#?}");
    // Still deleted on home, still waiting for its confirmation.
    assert_eq!(
        unit(&plan_of(&bed, &home, true), "work/r", UnitKind::Repo).decision,
        Decision::DeletedOn {
            host: "home".into()
        }
    );
}

#[test]
fn an_abort_during_a_deletion_note_begins_no_push() {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/r"));
    let home = bed.home();
    assert_eq!(yes(&bed, &home).outcome, Outcome::Done);
    std::fs::remove_dir_all(bed.leaf_hd.join("work/r")).unwrap();
    let before = (
        testutil::fingerprint(&bed.home_hd),
        testutil::fingerprint(&bed.home_dirs.state().join("markers")),
    );
    let (r, _) = raised_on_the_note(&bed, &home, "work/r");
    stopped_before(&r, "work/r");
    assert_eq!(
        (
            testutil::fingerprint(&bed.home_hd),
            testutil::fingerprint(&bed.home_dirs.state().join("markers")),
        ),
        before,
        "home's marker or the deletion was written"
    );
    assert!(
        !bed.home_dirs.state().join("pushes").exists()
            || testutil::fingerprint(&bed.home_dirs.state().join("pushes")).is_empty(),
        "a push was begun"
    );
    journaled(&home, "work/r");
}

#[test]
fn an_abort_raised_as_an_adoption_is_announced_records_no_baseline() {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/r"));
    let copy = std::process::Command::new("cp")
        .arg("-Rp")
        .arg(bed.home_hd.join("work/r"))
        .arg(bed.leaf_hd.join("work/r"))
        .status()
        .unwrap();
    assert!(copy.success());
    let home = bed.home();
    let (r, events) = sync_watched(&bed, &home, &|e, abort| {
        if matches!(e, ProgressEvent::UnitStarted { .. }) {
            abort.raise_because(HANGUP);
        }
    });
    stopped_before(&r, "work/r");
    assert_eq!(events.len(), 2, "{events:#?}");
    assert!(
        crate::baseline::store(&bed.leaf_dirs.state())
            .get::<Baseline>("work/r")
            .unwrap()
            .is_none()
    );
    assert!(
        leaf_baselines(&bed.home_dirs.state(), "leaf")
            .get::<Baseline>("work/r")
            .unwrap()
            .is_none()
    );
    // Adopted next time.
    assert_eq!(yes(&bed, &home).applied[0].action, "adopt");
}
