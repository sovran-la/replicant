//! PLAN.md rev 7: git-ignored files do not sync. Each side's ignored paths
//! are excluded from the dry run in both directions, so the source's are
//! never sent and the target's are never `--delete`d for being absent on the
//! source; guardrail 1 never sees them.

use super::*;

const IGNORE: &str = ".env*\ntarget/\n*.log\n!keep.log\n";

/// `work/r` on home with a committed .gitignore, and the leaf's copy of it.
fn pair(bed: &Bed) -> (PathBuf, PathBuf) {
    let h = testutil::repo(&bed.home_hd.join("work/r"));
    testutil::write(&h.join(".gitignore"), IGNORE);
    testutil::git(&h, &["add", ".gitignore"]);
    testutil::git(&h, &["commit", "-qm", "ignores"]);
    let l = bed.leaf_hd.join("work/r");
    copy(&h, &l);
    (h, l)
}

/// Baseline the repo as both sides hold it now, then run `change` and dry-run.
fn after(bed: &Bed, change: impl FnOnce()) -> PlannedUnit {
    let now = go(bed, &bed.home(), &bed.leaf_cfg(), false).unwrap();
    let r = unit(&now, "work/r");
    put_baseline(bed, "work/r", r.leaf.clone(), r.home.clone());
    change();
    unit(&dry(bed), "work/r").clone()
}

fn changes(u: &PlannedUnit) -> Vec<String> {
    u.diff.as_ref().expect("diffed").changes.clone()
}

fn names(u: &PlannedUnit, what: &str) -> bool {
    changes(u).iter().any(|c| c[2..].starts_with(what))
}

#[test]
fn a_pull_never_deletes_the_leafs_ignored_files() {
    let bed = Bed::enrolled();
    let (h, l) = pair(&bed);
    let u = after(&bed, || {
        // The leaf's own ignored files, which home has never seen.
        testutil::write(&l.join(".env.local"), "SECRET=leaf\n");
        testutil::write(&l.join("target/debug/app"), "binary");
        testutil::write(&l.join("sub/build.log"), "log");
        testutil::write(&h.join("new.txt"), "home's new file\n");
        testutil::git(&h, &["add", "new.txt"]);
        testutil::git(&h, &["commit", "-qm", "home moved"]);
    });
    assert_eq!(u.decision, Decision::Pull, "{u:#?}");
    assert!(names(&u, "new.txt"), "the pull itself: {:?}", changes(&u));
    for kept in [".env.local", "target", "sub/build.log"] {
        assert!(!names(&u, kept), "{kept} named: {:?}", changes(&u));
    }
    assert_eq!(u.diff.as_ref().unwrap().deleted, 0, "{:?}", changes(&u));
    // Guardrail 1 never sees an ignored file as uncommitted work.
    assert!(u.refusals.is_empty(), "{:?}", u.refusals);
    assert!(l.join(".env.local").exists(), "a dry run writes nothing");
}

#[test]
fn a_push_neither_sends_the_leafs_nor_deletes_homes_ignored_files() {
    let bed = Bed::enrolled();
    let (h, l) = pair(&bed);
    let u = after(&bed, || {
        testutil::write(&h.join(".env.home"), "SECRET=home\n");
        testutil::write(&h.join("target/release/app"), "binary");
        testutil::write(&l.join("target/debug/app"), "binary");
        testutil::write(&l.join("big.log"), "log");
        // Negated: untracked work, and it syncs.
        testutil::write(&l.join("keep.log"), "kept");
        testutil::write(&l.join("README"), "leaf's edit\n");
        testutil::git(&l, &["commit", "-qam", "leaf moved"]);
    });
    assert_eq!(u.decision, Decision::Push, "{u:#?}");
    assert!(names(&u, "README"), "{:?}", changes(&u));
    assert!(names(&u, "keep.log"), "negated: {:?}", changes(&u));
    for kept in [".env.home", "target", "big.log"] {
        assert!(!names(&u, kept), "{kept} named: {:?}", changes(&u));
    }
    assert_eq!(u.diff.as_ref().unwrap().deleted, 0, "{:?}", changes(&u));
    assert!(u.refusals.is_empty(), "{:?}", u.refusals);
}

#[test]
fn changing_only_ignored_files_changes_nothing() {
    let bed = Bed::enrolled();
    let (h, l) = pair(&bed);
    let u = after(&bed, || {
        testutil::write(&l.join(".env.local"), "SECRET=leaf\n");
        testutil::write(&h.join("target/x"), "home build");
    });
    assert_eq!(
        u.decision,
        Decision::Nothing {
            why: "unchanged".into()
        },
        "{u:#?}"
    );
}

/// The same name ignored on one side only (home's .gitignore moved on): the
/// side that ignores it keeps it, so it is excluded from the transfer.
#[test]
fn a_path_ignored_on_either_side_is_left_alone() {
    let bed = Bed::enrolled();
    let (h, _) = pair(&bed);
    let u = after(&bed, || {
        testutil::write(&h.join(".gitignore"), &format!("{IGNORE}notes/\n"));
        testutil::git(&h, &["commit", "-qam", "ignore notes"]);
        testutil::write(&h.join("notes/home.md"), "home's notes");
    });
    assert_eq!(u.decision, Decision::Pull, "{u:#?}");
    assert!(names(&u, ".gitignore"), "{:?}", changes(&u));
    assert!(!names(&u, "notes"), "{:?}", changes(&u));
}

/// A tracked file is never ignored, whatever a rule says: home's edit to a
/// force-added `.log` (and to one inside an ignored `gen/`) travels.
#[test]
fn tracked_files_matching_a_rule_still_sync() {
    let bed = Bed::enrolled();
    let h = testutil::repo(&bed.home_hd.join("work/r"));
    testutil::write(&h.join(".gitignore"), &format!("{IGNORE}gen/\n"));
    testutil::write(&h.join("forced.log"), "v1\n");
    testutil::write(&h.join("gen/keep.txt"), "v1\n");
    testutil::git(
        &h,
        &["add", "-f", ".gitignore", "forced.log", "gen/keep.txt"],
    );
    testutil::git(&h, &["commit", "-qm", "forced"]);
    let l = bed.leaf_hd.join("work/r");
    copy(&h, &l);
    let u = after(&bed, || {
        testutil::write(&l.join("gen/debug/app"), "leaf build");
        testutil::write(&h.join("forced.log"), "v2, longer\n");
        testutil::write(&h.join("gen/keep.txt"), "v2, longer\n");
        testutil::git(&h, &["commit", "-qam", "home moved"]);
    });
    assert_eq!(u.decision, Decision::Pull, "{u:#?}");
    assert!(names(&u, "forced.log"), "{:?}", changes(&u));
    assert!(names(&u, "gen/keep.txt"), "{:?}", changes(&u));
    assert!(!names(&u, "gen/debug"), "{:?}", changes(&u));
    assert_eq!(u.diff.as_ref().unwrap().deleted, 0, "{:?}", changes(&u));
}
