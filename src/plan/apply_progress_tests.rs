//! What a sync announces while it applies: one start and one end per unit
//! that does something, in plan order, with how long each took; nothing for
//! a plan that may not go. And an abort raised between units (the CLI gone
//! after one finished) begins no other.

use super::*;
use crate::marker::Direction;

/// `sync` as `leaf`, collecting every event; `on`
/// sees each as it is announced, with the sync's abort.
pub(super) fn sync_watched(
    bed: &Bed,
    home: &Home,
    on: &(dyn Fn(&ProgressEvent, &crate::abort::Abort) + Sync),
) -> (SyncReport, Vec<ProgressEvent>) {
    sync_watched_with(bed, home, &SyncOpts::default(), on)
}

/// [`sync_watched`], with `opts`.
pub(super) fn sync_watched_with(
    bed: &Bed,
    home: &Home,
    opts: &SyncOpts,
    on: &(dyn Fn(&ProgressEvent, &crate::abort::Abort) + Sync),
) -> (SyncReport, Vec<ProgressEvent>) {
    let link = InProcess {
        home,
        host: "leaf".into(),
        home_dir: bed.leaf_hd.clone(),
    };
    let cfg = bed.leaf_cfg();
    let dirs = bed.leaf_dirs.state();
    let seen = std::sync::Mutex::new(Vec::new());
    let abort = crate::abort::Abort::default();
    // A unit's live counters come between its start and its end; these
    // tests are about the starts and ends (`counters_tests` the counters).
    let emit = |e: ProgressEvent| {
        on(&e, &abort);
        if !matches!(e, ProgressEvent::UnitProgress { .. }) {
            seen.lock().unwrap().push(e);
        }
    };
    let progress = Progress::new(&emit, abort.clone());
    let ctx = ctx(bed, &cfg, &dirs);
    let r = sync_with_progress(&link, ctx, opts, &progress).unwrap();
    (r, seen.into_inner().unwrap())
}

use crate::plan::apply::sync_with as sync_with_progress;

/// The events pair up, start then end, one pair per applied unit in the
/// report's order (which is plan order), numbered 1.. of `total`.
fn pairs_match(r: &SyncReport, events: &[ProgressEvent], total: usize) {
    assert_eq!(events.len(), 2 * r.applied.len(), "{events:#?}");
    for (i, (pair, a)) in events.chunks(2).zip(&r.applied).enumerate() {
        let ProgressEvent::UnitStarted {
            rel,
            action,
            index,
            total: t,
            ..
        } = &pair[0]
        else {
            panic!("not a start: {:?}", pair[0])
        };
        assert_eq!((rel, action, *index, *t), (&a.rel, &a.action, i + 1, total));
        let ProgressEvent::UnitDone {
            rel,
            action,
            files_changed,
            deleted,
            elapsed_ms,
            push,
            outcome,
            ..
        } = &pair[1]
        else {
            panic!("not an end: {:?}", pair[1])
        };
        assert_eq!(
            (rel, action, *files_changed, *deleted, push, outcome),
            (
                &a.rel,
                &a.action,
                a.files_changed,
                a.deleted,
                &a.push,
                &UnitOutcome::Applied
            )
        );
        assert_eq!(
            *elapsed_ms, a.elapsed_ms,
            "the report's time is the event's"
        );
    }
    let units: u64 = r.applied.iter().map(|a| a.elapsed_ms).sum();
    assert!(units <= r.elapsed_ms, "{units} > {}", r.elapsed_ms);
    assert!(
        r.elapsed_ms > 0 && r.elapsed_ms < 120_000,
        "{}",
        r.elapsed_ms
    );
}

#[test]
fn each_unit_is_announced_as_it_starts_and_ends_in_plan_order() {
    let bed = Bed::enrolled();
    tree(&bed.home_hd.join("work"));
    let home = bed.home();
    let (r, events) = sync_watched(&bed, &home, &|_, _| {});
    assert_eq!(r.outcome, Outcome::Done);
    let rels: Vec<&str> = r.applied.iter().map(|a| a.rel.as_str()).collect();
    // Plan order: by path, a repo after the loose unit holding it.
    assert_eq!(
        rels,
        ["work", "work/alpha", "work/beta", "work/nested/gamma"]
    );
    pairs_match(&r, &events, 4);
    assert!(events.iter().all(|e| match e {
        ProgressEvent::UnitStarted { direction, .. }
        | ProgressEvent::UnitDone { direction, .. } => *direction == Some(Direction::Pull),
        // Counters carry no direction of their own: their unit's start did.
        ProgressEvent::UnitProgress { .. } => true,
    }));
    assert!(r.applied.iter().any(|a| a.files_changed > 0));

    // The journal's line for each unit says how long it took.
    let log = home.log(&Default::default()).unwrap();
    let alpha = log
        .iter()
        .find(|l| l.event == "sync" && l.path == "work/alpha")
        .unwrap();
    let words: Vec<&str> = alpha.detail.split(' ').collect();
    assert_eq!(
        words[..2],
        ["pull", &r.applied[1].files_changed.to_string()]
    );
    assert!(
        words[5].ends_with('s') && words[6] == "ok",
        "{}",
        alpha.detail
    );
    assert!(
        log.iter().any(|l| l.event == "sync"
            && l.detail.contains(" units, ")
            && l.detail.contains(" del in ")),
        "{log:#?}"
    );

    // Nothing left to do: nothing announced.
    let (r, events) = sync_watched(&bed, &home, &|_, _| {});
    assert!(r.applied.is_empty() && events.is_empty(), "{events:?}");
}

#[test]
fn an_adoption_is_announced_too() {
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
    let (r, events) = sync_watched(&bed, &home, &|_, _| {});
    assert_eq!(r.applied[0].action, "adopt");
    pairs_match(&r, &events, 1);
    assert!(matches!(
        &events[0],
        ProgressEvent::UnitStarted {
            direction: None,
            ..
        }
    ));
}

#[test]
fn a_plan_that_may_not_go_announces_nothing() {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/r"));
    let l = testutil::repo(&bed.leaf_hd.join("work/r"));
    testutil::git(&l, &["commit", "-q", "--allow-empty", "-m", "leaf only"]);
    let home = bed.home();
    let (r, events) = sync_watched(&bed, &home, &|e, _| panic!("announced {e:?}"));
    assert!(
        matches!(r.outcome, Outcome::Blocked { .. }),
        "{:?}",
        r.outcome
    );
    assert!(events.is_empty());
}

#[test]
fn an_abort_between_units_begins_no_other_and_writes_nothing_of_it() {
    let bed = Bed::enrolled();
    tree(&bed.home_hd.join("work"));
    let home = bed.home();
    let (r, events) = sync_watched(&bed, &home, &|e, abort| {
        if let ProgressEvent::UnitDone { rel, .. } = e
            && rel == "work/alpha"
        {
            abort.raise_because(crate::daemon::local::HANGUP);
        }
    });
    match &r.outcome {
        Outcome::Failed { rel, why } => {
            assert_eq!(rel, "work/beta");
            assert!(why.contains("the CLI hung up"), "{why}");
            assert!(why.contains("nothing of it written"), "{why}");
        }
        o => panic!("{o:?}"),
    }
    let rels: Vec<&str> = r.applied.iter().map(|a| a.rel.as_str()).collect();
    assert_eq!(rels, ["work", "work/alpha"]);
    pairs_match(&r, &events, 4);
    let markers: Vec<Marker> = crate::marker::store(&bed.leaf_dirs.state()).all().unwrap();
    assert!(markers.is_empty(), "not even a marker: {markers:?}");
    assert!(!bed.leaf_hd.join("work/beta").exists());
    assert!(!bed.leaf_hd.join("work/nested/gamma").exists());
    assert!(home.queue.view().is_empty(), "the entry was given back");
    // The next sync carries on from there.
    let r = yes(&bed, &home);
    assert_eq!(r.outcome, Outcome::Done);
    assert!(bed.leaf_hd.join("work/nested/gamma/README").exists());
}

#[test]
fn counters_run_between_a_units_start_and_end_and_end_at_its_totals() {
    let bed = Bed::enrolled();
    tree(&bed.home_hd.join("work"));
    let home = bed.home();
    let all = std::sync::Mutex::new(Vec::new());
    let (r, _) = sync_watched(&bed, &home, &|e, _| all.lock().unwrap().push(e.clone()));
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    let all = all.into_inner().unwrap();
    let mut running = None;
    let mut last: Option<(u64, u64, u64, u64)> = None;
    let mut ended = 0;
    for e in &all {
        match e {
            ProgressEvent::UnitStarted { rel, .. } => (running, last) = (Some(rel.clone()), None),
            ProgressEvent::UnitProgress {
                rel,
                files_done,
                files_total,
                bytes_done,
                bytes_total,
                ..
            } => {
                assert_eq!(Some(rel), running.as_ref());
                if let Some((f, _, b, _)) = last {
                    assert!(*files_done >= f && *bytes_done >= b, "monotonic: {all:#?}");
                }
                last = Some((*files_done, *files_total, *bytes_done, *bytes_total));
            }
            ProgressEvent::UnitDone { files_changed, .. } => {
                if let Some((f, ft, b, bt)) = last {
                    assert_eq!((f, b), (ft, bt), "the last counters are the totals");
                    assert_eq!(f as usize, *files_changed);
                    ended += 1;
                }
                running = None;
            }
        }
    }
    assert!(ended >= 3, "every unit that sent files reported: {all:#?}");
}
