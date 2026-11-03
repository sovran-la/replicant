//! A leaf's marker resumes its own unit and nothing else (review A1). A loose
//! unit's marker covers every repo under it, and a repo's marker sits inside
//! its loose unit, but neither says anything about the other's direction:
//! resuming a neighbour would pull home's copy over leaf-only work, and a
//! leaf-only repo would be deleted outright.

use super::*;
use crate::marker::Direction;

/// The real transfer of the unit at `suffix` (`/work/` is the loose unit,
/// `/work/alpha/` the repo) fails without writing anything, on the suite's
/// plane (`tests_inject`).
pub(crate) fn fail_at(bed: &Bed, suffix: &str) -> crate::plan::tests::inject::Inject {
    let unit = suffix.trim_matches('/');
    crate::plan::tests::inject::Inject::new(bed, unit, crate::plan::tests::inject::How::Fails)
}

/// A clean first sync (a pull of the tree from home), then home changes
/// `home_edit` and the next sync's pull of the unit at `suffix` is
/// interrupted, leaving the leaf's marker for `rel`. The marker starts in a
/// later second than the first sync's writes, so guardrail 6 sees those as
/// older than the interruption, as they would be outside a test.
pub(crate) fn interrupted(rel: &str, suffix: &str, home_edit: &str) -> (Bed, Home) {
    let bed = Bed::enrolled();
    tree(&bed.home_hd.join("work"));
    let home = bed.home();
    assert_eq!(yes(&bed, &home).outcome, Outcome::Done);
    testutil::next_second();
    let edited = bed.home_hd.join(home_edit);
    testutil::write(&edited, "home edit\n");
    if home_edit.starts_with("work/alpha/") {
        testutil::git(&bed.home_hd.join("work/alpha"), &["commit", "-qam", "home"]);
    }
    let r = sync_inject(&bed, &home, &SyncOpts::default(), &fail_at(&bed, suffix)).unwrap();
    assert!(
        matches!(&r.outcome, Outcome::Interrupted { rel: r, .. } if r == rel),
        "{:#?}",
        r.outcome
    );
    let m: Vec<Marker> = crate::marker::store(&bed.leaf_dirs.state()).all().unwrap();
    assert_eq!(
        m.iter().map(|m| m.rel.as_str()).collect::<Vec<_>>(),
        [rel],
        "the leaf's marker"
    );
    (bed, home)
}

fn head(repo: &Path) -> String {
    testutil::git(repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string()
}

#[test]
fn a_loose_units_marker_does_not_pull_over_a_leaf_only_commit_in_a_repo_inside_it() {
    let (bed, home) = interrupted("work", "/work/", "work/notes.txt");
    let alpha = bed.leaf_hd.join("work/alpha");
    testutil::write(&alpha.join("mine.txt"), "committed on the leaf only\n");
    testutil::git(&alpha, &["add", "mine.txt"]);
    testutil::git(&alpha, &["commit", "-qm", "leaf only"]);
    let mine = head(&alpha);

    let p = plan_of(&bed, &home, true);
    assert_eq!(
        unit(&p, "work/alpha", UnitKind::Repo).decision,
        Decision::Push
    );
    assert_eq!(
        unit(&p, "work", UnitKind::Loose).decision,
        Decision::Resume {
            direction: Direction::Pull
        },
        "its own marker still resumes"
    );
    let r = yes(&bed, &home);
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    assert_eq!(head(&alpha), mine, "the leaf's commit stands");
    assert!(alpha.join("mine.txt").exists());
    assert_eq!(
        head(&bed.home_hd.join("work/alpha")),
        mine,
        "and went to home"
    );
    assert_eq!(
        std::fs::read_to_string(bed.leaf_hd.join("work/notes.txt")).unwrap(),
        "home edit\n",
        "the loose unit's own resume finished"
    );
}

#[test]
fn a_repos_marker_does_not_revert_a_leaf_only_edit_in_its_loose_unit() {
    let (bed, home) = interrupted("work/alpha", "/work/alpha/", "work/alpha/README");
    let notes = bed.leaf_hd.join("work/notes.txt");
    testutil::write(&notes, "edited on the leaf only\n");

    let p = plan_of(&bed, &home, true);
    assert_eq!(unit(&p, "work", UnitKind::Loose).decision, Decision::Push);
    assert_eq!(
        unit(&p, "work/alpha", UnitKind::Repo).decision,
        Decision::Resume {
            direction: Direction::Pull
        }
    );
    let r = yes(&bed, &home);
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    assert_eq!(
        std::fs::read_to_string(&notes).unwrap(),
        "edited on the leaf only\n"
    );
    assert_eq!(
        std::fs::read_to_string(bed.home_hd.join("work/notes.txt")).unwrap(),
        "edited on the leaf only\n",
        "pushed"
    );
}

#[test]
fn a_loose_units_marker_does_not_delete_a_new_leaf_only_repo() {
    let (bed, home) = interrupted("work", "/work/", "work/notes.txt");
    let fresh = testutil::repo(&bed.leaf_hd.join("work/fresh"));
    let before = testutil::fingerprint(&fresh);

    let p = plan_of(&bed, &home, true);
    let d = &unit(&p, "work/fresh", UnitKind::Repo).decision;
    assert_ne!(
        *d,
        Decision::Resume {
            direction: Direction::Pull
        }
    );
    assert_eq!(*d, Decision::Push, "no baseline, home empty: a push");
    let r = yes(&bed, &home);
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    assert!(fresh.join("README").exists());
    assert_eq!(
        testutil::fingerprint(&fresh),
        before,
        "the leaf's new repo is untouched"
    );
    assert!(bed.home_hd.join("work/fresh/README").exists(), "and pushed");
}

#[test]
fn an_own_push_marker_on_home_does_not_push_its_loose_unit_over_a_home_only_change() {
    // The mirror on home: this leaf's interrupted push of `work/alpha` is
    // marked on home; a change made on home alone to the loose `work` must
    // come down (a pull), not be pushed over by a resume of `work`.
    let bed = Bed::enrolled();
    tree(&bed.leaf_hd.join("work"));
    let home = bed.home();
    assert_eq!(yes(&bed, &home).outcome, Outcome::Done);
    testutil::write(&bed.leaf_hd.join("work/alpha/README"), "leaf edit\n");
    let r = sync_inject(
        &bed,
        &home,
        &SyncOpts::default(),
        &fail_at(&bed, "/work/alpha/"),
    )
    .unwrap();
    assert!(matches!(&r.outcome, Outcome::Interrupted { rel, .. } if rel == "work/alpha"));
    let on_home: Vec<Marker> = crate::marker::store(&bed.home_dirs.state()).all().unwrap();
    assert_eq!(on_home.len(), 1);
    assert_eq!(on_home[0].rel, "work/alpha");

    testutil::write(&bed.home_hd.join("work/notes.txt"), "edited on home only\n");
    let p = plan_of(&bed, &home, true);
    assert_eq!(unit(&p, "work", UnitKind::Loose).decision, Decision::Pull);
    assert_eq!(
        unit(&p, "work/alpha", UnitKind::Repo).decision,
        Decision::Resume {
            direction: Direction::Push
        }
    );
    let r = yes(&bed, &home);
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    for side in [&bed.home_hd, &bed.leaf_hd] {
        assert_eq!(
            std::fs::read_to_string(side.join("work/notes.txt")).unwrap(),
            "edited on home only\n"
        );
    }
}

/// The review's h1: a repo pull cut after a first round's `.git` content
/// arrived and before the worktree file did. That state is placed by hand,
/// from what the transfer's own first round holds, so it is whatever this
/// transfer can really leave. An index in that round makes `git status` call
/// the not-yet-arrived file dirty, and guardrail 1 refuses the resume.
#[test]
fn a_repo_pull_cut_before_its_worktree_files_resumes_rather_than_blocks() {
    use crate::native::{Manifest, diff, local, workers};
    let (bed, home) = interrupted("work/alpha", "/work/alpha/", "work/alpha/README");
    let (from, to) = (
        bed.home_hd.join("work/alpha"),
        bed.leaf_hd.join("work/alpha"),
    );
    let none = crate::exclude::Excludes::default();
    let source = Manifest::walk(&from, &none, false).unwrap();
    let target = Manifest::walk(&to, &none, false).unwrap();
    let changes = diff(&source, &target);
    let [first, _] = workers::rounds(&changes.send);
    let arrived: Vec<String> = first
        .into_iter()
        .flat_map(|item| match item {
            workers::Item::Batch(rels) => rels,
            workers::Item::Big(rel) => vec![rel],
        })
        .filter(|rel| rel.starts_with(".git/"))
        .collect();
    assert!(
        arrived.iter().any(|r| r.starts_with(".git/objects/")),
        "{arrived:?}"
    );
    local::copy_listed(&from, &to, &arrived).unwrap();
    let readme = std::fs::read(to.join("README")).unwrap();
    assert_ne!(
        readme,
        std::fs::read(from.join("README")).unwrap(),
        "not yet"
    );

    let r = yes(&bed, &home);
    assert_eq!(r.outcome, Outcome::Done, "resumes: {:#?}", r.plan.units);
    assert_eq!(
        std::fs::read(to.join("README")).unwrap(),
        std::fs::read(from.join("README")).unwrap()
    );
    assert_eq!(head(&to), head(&from));
    assert_eq!(testutil::git(&to, &["status", "--porcelain"]), "");
}
