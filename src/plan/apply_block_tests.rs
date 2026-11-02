//! When `sync` writes nothing at all: a blocking unit, a refusal, a
//! deletion nobody confirmed, a plan that changed since it was confirmed.
//! And the two removals that do write: a confirmed deletion, an unsubscribe.

use super::*;
use crate::daemon::wire::{HomeReply, HomeRequest};
use crate::plan::HomeLink;

/// Nothing changed anywhere: both trees, markers, baselines on both sides.
fn untouched(bed: &Bed, run: impl FnOnce() -> SyncReport) -> SyncReport {
    let before = (
        testutil::fingerprint(&bed.home_hd),
        testutil::fingerprint(&bed.leaf_hd),
        testutil::fingerprint(&bed.leaf_dirs.state()),
        testutil::fingerprint(&bed.home_dirs.state().join("markers")),
        testutil::fingerprint(&bed.home_dirs.state().join("leaf-baselines")),
    );
    let r = run();
    let after = (
        testutil::fingerprint(&bed.home_hd),
        testutil::fingerprint(&bed.leaf_hd),
        testutil::fingerprint(&bed.leaf_dirs.state()),
        testutil::fingerprint(&bed.home_dirs.state().join("markers")),
        testutil::fingerprint(&bed.home_dirs.state().join("leaf-baselines")),
    );
    assert_eq!(before, after, "nothing may be written: {r:#?}");
    assert!(r.applied.is_empty());
    r
}

fn blocked_why(r: &SyncReport) -> String {
    match &r.outcome {
        Outcome::Blocked { why } => why.join("\n"),
        o => panic!("not blocked: {o:?}"),
    }
}

#[test]
fn a_conflict_blocks_the_whole_sync() {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/r"));
    let l = testutil::repo(&bed.leaf_hd.join("work/r"));
    testutil::git(&l, &["commit", "-q", "--allow-empty", "-m", "leaf only"]);
    // Something else would pull, and must not.
    testutil::write(&bed.home_hd.join("work/notes.txt"), "would pull\n");
    let home = bed.home();
    let r = untouched(&bed, || yes(&bed, &home));
    let why = blocked_why(&r);
    assert!(why.contains("work/r: conflict"), "{why}");
    assert_eq!(r.outcome.exit_code(), 2);
    let log = home.log(&Default::default()).unwrap();
    assert!(log.last().unwrap().detail.contains("nothing written"));
}

#[test]
fn a_guardrail_refusal_blocks_the_whole_sync() {
    let bed = Bed::enrolled();
    let beta = testutil::repo(&bed.home_hd.join("work/beta"));
    testutil::write(&beta.join(".git/index.lock"), "");
    testutil::write(&bed.home_hd.join("work/notes.txt"), "would pull\n");
    let home = bed.home();
    let why = blocked_why(&untouched(&bed, || yes(&bed, &home)));
    assert!(why.contains("work/beta"), "{why}");
}

/// Pull a repo, then delete it on the leaf: "deleted on leaf".
fn deleted_on_leaf(bed: &Bed, home: &Home) {
    let r = testutil::repo(&bed.home_hd.join("work/r"));
    testutil::write(&r.join(".gitignore"), "secret.env\n");
    testutil::git(&r, &["add", ".gitignore"]);
    testutil::git(&r, &["commit", "-qm", "ignore"]);
    testutil::write(&r.join("secret.env"), "home's ignored file\n");
    assert_eq!(yes(bed, home).outcome, Outcome::Done);
    std::fs::remove_dir_all(bed.leaf_hd.join("work/r")).unwrap();
    let p = plan_of(bed, home, false);
    assert_eq!(
        unit(&p, "work/r", UnitKind::Repo).decision,
        Decision::DeletedOn {
            host: "leaf".into()
        }
    );
}

#[test]
fn a_deletion_needs_its_confirmation_and_yes_is_not_one() {
    let bed = Bed::enrolled();
    let home = bed.home();
    deleted_on_leaf(&bed, &home);
    let why = blocked_why(&untouched(&bed, || yes(&bed, &home)));
    assert!(why.contains("--confirm-delete work/r"), "{why}");
    // A confirmation naming nothing deleted is refused, not ignored.
    let opts = SyncOpts {
        confirm_delete: vec!["work/r".into(), "work/elsewhere".into()],
        ..Default::default()
    };
    let r = untouched(&bed, || {
        sync_with(&bed, &home, &opts, crate::plan::RSYNC).unwrap()
    });
    assert!(blocked_why(&r).contains("--confirm-delete work/elsewhere"));
}

#[test]
fn a_confirmed_deletion_on_the_leaf_deletes_home_and_drops_the_baselines() {
    let bed = Bed::enrolled();
    let home = bed.home();
    deleted_on_leaf(&bed, &home);
    let opts = SyncOpts {
        // This leaf's own path for it works as well as the unit path.
        confirm_delete: vec![format!("{}/", bed.leaf_hd.join("work/r").display())],
        ..Default::default()
    };
    let before = testutil::fingerprint(&bed.home_hd);
    let r = sync_with(&bed, &home, &opts, crate::plan::RSYNC).unwrap();
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    assert_eq!(r.applied[0].action, "delete on home");
    let gone = changed(&before, &testutil::fingerprint(&bed.home_hd));
    assert_eq!(gone, itemized(&r));
    assert!(gone.contains("work/r/README") && gone.contains("work/r/.git/HEAD"));
    assert_eq!(
        std::fs::read_to_string(bed.home_hd.join("work/r/secret.env")).unwrap(),
        "home's ignored file\n",
        "ignored files are never moved or deleted"
    );
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
    let log = home.log(&Default::default()).unwrap();
    let confirm = log.iter().find(|l| l.event == "confirm-delete").unwrap();
    assert_eq!(confirm.path, "work/r");
    assert!(
        confirm.detail.contains("would remove"),
        "{}",
        confirm.detail
    );
    assert!(markers(&bed.home_dirs.state()).is_empty());
}

#[test]
fn a_confirmed_deletion_on_home_deletes_the_leaf_copy() {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/r"));
    let home = bed.home();
    assert_eq!(yes(&bed, &home).outcome, Outcome::Done);
    std::fs::remove_dir_all(bed.home_hd.join("work/r")).unwrap();
    let opts = SyncOpts {
        confirm_delete: vec!["~/work/r".into()],
        ..Default::default()
    };
    let before = testutil::fingerprint(&bed.leaf_hd);
    let r = sync_with(&bed, &home, &opts, crate::plan::RSYNC).unwrap();
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    assert_eq!(r.applied[0].action, "delete here");
    let gone = changed(&before, &testutil::fingerprint(&bed.leaf_hd));
    assert_eq!(gone, itemized(&r));
    assert!(!bed.leaf_hd.join("work/r/README").exists());
    assert!(markers(&bed.leaf_dirs.state()).is_empty());
}

#[test]
fn a_plan_that_changed_since_it_was_confirmed_writes_nothing() {
    let bed = Bed::enrolled();
    tree(&bed.home_hd.join("work"));
    let home = bed.home();
    let seen = plan_of(&bed, &home, true);
    testutil::write(
        &bed.home_hd.join("work/notes.txt"),
        "edited after the prompt\n",
    );
    let opts = SyncOpts {
        expect: Some(seen.digest()),
        ..Default::default()
    };
    let r = untouched(&bed, || {
        sync_with(&bed, &home, &opts, crate::plan::RSYNC).unwrap()
    });
    assert_eq!(r.outcome, Outcome::Changed);
    assert_eq!(r.outcome.exit_code(), 1);

    // Confirmed against what it is now, it goes.
    let opts = SyncOpts {
        expect: Some(plan_of(&bed, &home, true).digest()),
        ..Default::default()
    };
    let r = sync_with(&bed, &home, &opts, crate::plan::RSYNC).unwrap();
    assert_eq!(r.outcome, Outcome::Done);
}

#[test]
fn an_unsubscribed_unit_is_removed_with_its_ignored_files_spared() {
    let bed = Bed::enrolled();
    let r = testutil::repo(&bed.home_hd.join("work/r"));
    testutil::write(&r.join(".gitignore"), ".env.local\n");
    testutil::git(&r, &["add", ".gitignore"]);
    testutil::git(&r, &["commit", "-qm", "ignore"]);
    let home = bed.home();
    assert_eq!(yes(&bed, &home).outcome, Outcome::Done);
    testutil::write(&bed.leaf_hd.join("work/r/.env.local"), "leaf secret\n");
    let link = InProcess {
        home: &home,
        host: "leaf".into(),
        home_dir: bed.leaf_hd.clone(),
    };
    let leaf_r = bed.leaf_hd.join("work/r").display().to_string();
    let reply = link
        .call(HomeRequest::Unsubscribe {
            paths: vec![leaf_r],
        })
        .unwrap();
    assert!(matches!(reply, HomeReply::Unsubscribed(_)), "{reply:?}");
    let home_before = testutil::fingerprint(&bed.home_hd);
    let before = testutil::fingerprint(&bed.leaf_hd);
    let rep = yes(&bed, &home);
    assert_eq!(rep.outcome, Outcome::Done, "{rep:#?}");
    assert_eq!(rep.applied[0].action, "remove");
    assert_eq!(testutil::fingerprint(&bed.home_hd), home_before);
    let gone = changed(&before, &testutil::fingerprint(&bed.leaf_hd));
    assert_eq!(gone, itemized(&rep));
    assert!(!bed.leaf_hd.join("work/r/README").exists());
    assert_eq!(
        std::fs::read_to_string(bed.leaf_hd.join("work/r/.env.local")).unwrap(),
        "leaf secret\n"
    );
    assert!(
        crate::baseline::store(&bed.leaf_dirs.state())
            .get::<Baseline>("work/r")
            .unwrap()
            .is_none(),
        "the path leaves the table"
    );
    assert!(markers(&bed.leaf_dirs.state()).is_empty());
}
