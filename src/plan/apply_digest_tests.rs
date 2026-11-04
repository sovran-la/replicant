//! A confirmed plan against the plan made again at apply time
//! (`Plan::digest`): bytes that changed under the person still go, since
//! a push or pull copies what is there when it runs; a unit more or fewer,
//! a decision flipped, or one path more to delete stops the whole sync
//! before anything is written.

use super::block::untouched;
use super::*;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// `sync` as `leaf`, confirmed against `seen`.
fn confirmed(bed: &Bed, home: &Home, seen: &Plan) -> SyncReport {
    let opts = SyncOpts {
        expect: Some(seen.digest()),
        ..Default::default()
    };
    sync_with(bed, home, &opts).unwrap()
}

/// Set `path`'s mtime an hour back, to the whole second: a touch that
/// changes no content.
fn touch(path: &Path) {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_secs(now.as_secs() - 3600))
        .unwrap();
}

/// Home's `work/r` (two tracked files) and `work/notes.txt`, synced to the
/// leaf.
fn synced(bed: &Bed, home: &Home) {
    let r = testutil::repo(&bed.home_hd.join("work/r"));
    testutil::write(&r.join("src.txt"), "source\n");
    testutil::git(&r, &["add", "src.txt"]);
    testutil::git(&r, &["commit", "-qm", "src"]);
    testutil::write(&bed.home_hd.join("work/notes.txt"), "loose\n");
    assert_eq!(yes(bed, home).outcome, Outcome::Done);
}

#[test]
fn content_edited_and_files_touched_after_the_prompt_still_go() {
    let bed = Bed::enrolled();
    let home = bed.home();
    synced(&bed, &home);
    let leaf_r = bed.leaf_hd.join("work/r");
    testutil::write(&bed.leaf_hd.join("work/notes.txt"), "edited here\n");
    testutil::write(&leaf_r.join("README"), "edited here, uncommitted\n");
    let seen = plan_of(&bed, &home, true);
    assert_eq!(
        unit(&seen, "work", UnitKind::Loose).decision,
        Decision::Push
    );
    assert_eq!(
        unit(&seen, "work/r", UnitKind::Repo).decision,
        Decision::Push
    );

    // While the person reads the plan: an editor saves again, and something
    // touches a tracked file it does not change.
    let latest = "edited again while the plan was on screen\n";
    testutil::write(&bed.leaf_hd.join("work/notes.txt"), latest);
    touch(&leaf_r.join("src.txt"));
    let now = plan_of(&bed, &home, true);
    assert_ne!(
        unit(&now, "work", UnitKind::Loose).leaf,
        unit(&seen, "work", UnitKind::Loose).leaf,
        "the loose unit's state moved"
    );
    assert_ne!(
        unit(&now, "work/r", UnitKind::Repo).diff,
        unit(&seen, "work/r", UnitKind::Repo).diff,
        "the repo's change list moved"
    );

    let r = confirmed(&bed, &home, &seen);
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    let rels: Vec<&str> = r.applied.iter().map(|a| a.rel.as_str()).collect();
    assert_eq!(rels, ["work", "work/r"]);
    assert_eq!(
        std::fs::read_to_string(bed.home_hd.join("work/notes.txt")).unwrap(),
        latest,
        "what lands is what is there when it runs"
    );
    assert_eq!(
        std::fs::read_to_string(bed.home_hd.join("work/r/README")).unwrap(),
        "edited here, uncommitted\n"
    );
    let mtime = |p: &Path| std::fs::metadata(p).unwrap().modified().unwrap();
    assert_eq!(
        mtime(&bed.home_hd.join("work/r/src.txt")),
        mtime(&leaf_r.join("src.txt"))
    );
    assert!(
        plan_of(&bed, &home, true)
            .units
            .iter()
            .all(|u| matches!(u.decision, Decision::Nothing { .. })),
        "in step afterwards"
    );
}

#[test]
fn a_new_unit_since_the_prompt_writes_nothing() {
    let bed = Bed::enrolled();
    tree(&bed.home_hd.join("work"));
    let home = bed.home();
    let seen = plan_of(&bed, &home, true);
    testutil::repo(&bed.leaf_hd.join("work/new"));
    let r = untouched(&bed, || confirmed(&bed, &home, &seen));
    assert_eq!(r.outcome, Outcome::Changed);
    assert_eq!(r.outcome.exit_code(), 1);
}

#[test]
fn one_more_path_to_delete_since_the_prompt_writes_nothing() {
    let bed = Bed::enrolled();
    let home = bed.home();
    synced(&bed, &home);
    let r = bed.home_hd.join("work/r");
    testutil::write(&r.join("README"), "home's next\n");
    testutil::git(&r, &["commit", "-qam", "next"]);
    let seen = plan_of(&bed, &home, true);
    let planned = unit(&seen, "work/r", UnitKind::Repo);
    assert_eq!(planned.decision, Decision::Pull);
    assert_eq!(planned.diff.as_ref().unwrap().deleted, 0);

    // Something the repo's state does not cover (a fetch's FETCH_HEAD),
    // which the pull would delete: the same decision, one delete more.
    testutil::write(&bed.leaf_hd.join("work/r/.git/FETCH_HEAD"), "fetched\n");
    let now = plan_of(&bed, &home, true);
    let u = unit(&now, "work/r", UnitKind::Repo);
    assert_eq!(u.decision, Decision::Pull);
    assert!(
        u.diff
            .as_ref()
            .unwrap()
            .changes
            .contains(&"- .git/FETCH_HEAD".to_string()),
        "{:?}",
        u.diff
    );

    let r = untouched(&bed, || confirmed(&bed, &home, &seen));
    assert_eq!(r.outcome, Outcome::Changed);
    // Shown the plan with the deletion, the person can agree to it.
    let r = confirmed(&bed, &home, &now);
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    assert!(!bed.leaf_hd.join("work/r/.git/FETCH_HEAD").exists());
}

#[test]
fn a_decision_flipped_since_the_prompt_writes_nothing() {
    let bed = Bed::enrolled();
    let home = bed.home();
    synced(&bed, &home);
    testutil::write(&bed.home_hd.join("work/notes.txt"), "home's next\n");
    let seen = plan_of(&bed, &home, true);
    assert_eq!(
        unit(&seen, "work", UnitKind::Loose).decision,
        Decision::Pull
    );
    assert!(matches!(
        unit(&seen, "work/r", UnitKind::Repo).decision,
        Decision::Nothing { .. }
    ));
    // A commit here: work/r goes from nothing to a push.
    let leaf_r = bed.leaf_hd.join("work/r");
    testutil::git(&leaf_r, &["commit", "-q", "--allow-empty", "-m", "here"]);
    let r = untouched(&bed, || confirmed(&bed, &home, &seen));
    assert_eq!(r.outcome, Outcome::Changed);
}

#[test]
fn a_pull_turned_conflict_since_the_prompt_blocks_and_writes_nothing() {
    let bed = Bed::enrolled();
    let home = bed.home();
    synced(&bed, &home);
    let r = bed.home_hd.join("work/r");
    testutil::write(&r.join("README"), "home's next\n");
    testutil::git(&r, &["commit", "-qam", "next"]);
    let seen = plan_of(&bed, &home, true);
    assert_eq!(
        unit(&seen, "work/r", UnitKind::Repo).decision,
        Decision::Pull
    );
    // Uncommitted work here, which the pull would overwrite. A plan that may
    // not go at all says why (exit 2), rather than only that it changed.
    testutil::write(&bed.leaf_hd.join("work/r/src.txt"), "work in progress\n");
    let r = untouched(&bed, || confirmed(&bed, &home, &seen));
    match &r.outcome {
        Outcome::Blocked { why } => {
            assert!(why.join("\n").contains("work/r: conflict"), "{why:?}")
        }
        o => panic!("{o:?}"),
    }
    assert_eq!(r.outcome.exit_code(), 2);
}

#[test]
fn a_unit_refused_since_the_prompt_blocks_and_writes_nothing() {
    let bed = Bed::enrolled();
    let home = bed.home();
    synced(&bed, &home);
    let r = bed.home_hd.join("work/r");
    testutil::write(&r.join("README"), "home's next\n");
    testutil::git(&r, &["commit", "-qam", "next"]);
    let seen = plan_of(&bed, &home, true);
    let u = unit(&seen, "work/r", UnitKind::Repo);
    assert_eq!(u.decision, Decision::Pull);
    assert!(u.refusals.is_empty());
    // A git write begins on home in the repo being pulled: its lock is held
    // (guardrail 3). The same decision, a refusal more.
    testutil::write(&r.join(".git/index.lock"), "");
    let now = plan_of(&bed, &home, true);
    let u = unit(&now, "work/r", UnitKind::Repo);
    assert_eq!(u.decision, Decision::Pull);
    assert!(!u.refusals.is_empty());
    assert_ne!(now.digest(), seen.digest());
    let r = untouched(&bed, || confirmed(&bed, &home, &seen));
    match &r.outcome {
        Outcome::Blocked { why } => assert!(why.join("\n").contains("work/r"), "{why:?}"),
        o => panic!("{o:?}"),
    }
}
