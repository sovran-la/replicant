//! Guardrail 1 against the baseline. Dirt both sides agreed on at the last
//! sync is the source's own earlier work, not the target's: a repo synced
//! with uncommitted work refused every later push of it (thistledown's
//! `mikrotik` `Cargo.lock`, 2026-11-14). Where the guard stays whole
//! (removal), a stale index is still not work (rev 18).

use super::*;

/// A repo on both sides, synced with an uncommitted `Cargo.lock` edit, and
/// the baseline that sync would have written. Returns (home, leaf).
fn synced_dirty(bed: &Bed) -> (PathBuf, PathBuf) {
    let h = testutil::repo(&bed.home_hd.join("work/r"));
    testutil::write(&h.join("Cargo.lock"), "v1\n");
    testutil::git(&h, &["add", "Cargo.lock"]);
    testutil::git(&h, &["commit", "-qm", "lock"]);
    testutil::write(&h.join("Cargo.lock"), "v2, never committed\n");
    let l = bed.leaf_hd.join("work/r");
    copy(&h, &l);
    let s = go(bed, &bed.home(), &bed.leaf_cfg(), false).unwrap();
    let u = unit(&s, "work/r");
    assert_eq!(
        u.decision,
        Decision::Nothing {
            why: "identical".into()
        },
        "the fixture is a finished sync"
    );
    put_baseline(bed, "work/r", u.leaf.clone(), u.home.clone());
    (h, l)
}

#[test]
fn a_push_over_dirt_the_last_sync_carried_is_not_refused() {
    let bed = Bed::enrolled();
    let (_, l) = synced_dirty(&bed);
    testutil::write(&l.join("Cargo.lock"), "v3, the leaf moved on\n");
    let p = dry(&bed);
    let r = unit(&p, "work/r");
    assert_eq!(r.decision, Decision::Push);
    assert!(r.refusals.is_empty(), "{:?}", r.refusals);
    let d = r.diff.as_ref().expect("diffed");
    assert!(
        d.changes.iter().any(|c| c.ends_with(" Cargo.lock")),
        "the push still carries it: {d:?}"
    );
}

#[test]
fn a_pull_onto_dirt_the_last_sync_carried_is_not_refused() {
    let bed = Bed::enrolled();
    let (h, _) = synced_dirty(&bed);
    testutil::write(&h.join("Cargo.lock"), "v3, home moved on\n");
    let p = dry(&bed);
    let r = unit(&p, "work/r");
    assert_eq!(r.decision, Decision::Pull);
    assert!(r.refusals.is_empty(), "{:?}", r.refusals);
}

/// Real work on the target since the baseline never reaches guardrail 1 on
/// a push: the table calls it a conflict and nothing is diffed.
#[test]
fn target_work_since_the_baseline_is_a_conflict_not_a_waived_push() {
    let bed = Bed::enrolled();
    let (h, l) = synced_dirty(&bed);
    testutil::write(&l.join("Cargo.lock"), "v3, the leaf moved on\n");
    testutil::write(&h.join("Cargo.lock"), "v3, so did home\n");
    let p = dry(&bed);
    let r = unit(&p, "work/r");
    assert!(
        matches!(&r.decision, Decision::Conflict { why } if why == "both changed since baseline"),
        "{:?}",
        r.decision
    );
    assert!(r.diff.is_none());
    assert!(p.blocked());
}

/// A leaf repo set up by `setup`, baselined as it stands, then
/// unsubscribed: the removal's plan.
fn removed_with(setup: impl FnOnce(&Path)) -> PlannedUnit {
    let bed = Bed::enrolled();
    let old = testutil::repo(&bed.leaf_hd.join("work/old"));
    setup(&old);
    let now = unit(
        &go(&bed, &bed.home(), &bed.leaf_cfg(), false).unwrap(),
        "work/old",
    )
    .leaf
    .clone();
    let text = std::fs::read_to_string(bed.home_dirs.fleet()).unwrap();
    std::fs::write(
        bed.home_dirs.fleet(),
        text.replace(
            "paths = [\"work\"]",
            "paths = [\"work\"]\nexcept = [\"work/old\"]",
        ),
    )
    .unwrap();
    put_baseline(&bed, "work/old", now, UnitState::Present("y".into()));
    let p = dry(&bed);
    let r = unit(&p, "work/old").clone();
    assert_eq!(r.decision, Decision::Remove);
    r
}

/// A phantom: the index says README is modified, the file on disk is
/// HEAD's (an index carried over from another machine; thistledown's
/// `tundra-tools`, 2026-11-05). Removal keeps guardrail 1 whole even at the
/// baseline, so this is where the phantom is still told from work.
#[test]
fn a_stale_index_is_not_uncommitted_work() {
    let r = removed_with(|old| {
        testutil::write(&old.join("README"), "staged, never committed\n");
        testutil::git(old, &["add", "README"]);
        testutil::write(&old.join("README"), "hello\n");
    });
    assert!(r.refusals.is_empty(), "{:?}", r.refusals);

    // The same, with the staged content left on disk: that is work.
    let r = removed_with(|old| {
        testutil::write(&old.join("README"), "staged, never committed\n");
        testutil::git(old, &["add", "README"]);
    });
    assert_eq!(r.refusals[0].guard, Guard::DirtyTarget);
    assert_eq!(r.refusals[0].detail, vec!["README"]);
}
