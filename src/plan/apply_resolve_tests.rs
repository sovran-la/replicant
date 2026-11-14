//! A conflict resolved by hand (rev 22): taking this leaf's copy pushes it
//! over home's, which home keeps in the push's backup; taking home's pulls
//! it over the leaf's, which the leaf keeps in a backup of its own, through
//! an interruption and its resume too. A skipped conflict is left alone
//! and the rest of the plan goes. An answer that no longer fits the plan
//! writes nothing.

use super::*;
use crate::decide::{Resolution, Take};
use crate::marker::Direction;
use crate::plan::tests::inject::{How, Inject};
use std::path::PathBuf;

/// A repo synced once, then changed on both sides: the leaf committed and
/// left untracked work, home left its README and a coverage file
/// uncommitted. Returns (bed, home, home's repo, the leaf's).
fn conflicted() -> (Bed, Home, PathBuf, PathBuf) {
    let bed = Bed::enrolled();
    let h = testutil::repo(&bed.home_hd.join("work/r"));
    let home = bed.home();
    assert_eq!(yes(&bed, &home).outcome, Outcome::Done);
    let l = bed.leaf_hd.join("work/r");
    testutil::write(&l.join("README"), "the leaf's newer README\n");
    testutil::git(&l, &["commit", "-qam", "leaf moved"]);
    testutil::write(&l.join("scratch.txt"), "the leaf's uncommitted work\n");
    testutil::write(&h.join("README"), "home's uncommitted README\n");
    testutil::write(&h.join("default_1.profraw"), "coverage\n");
    // The interruption below starts in a later second than this work, as it
    // would outside a test (guardrail 6 compares whole seconds).
    testutil::next_second();
    (bed, home, h, l)
}

fn take(rel: &str, take: Take) -> SyncOpts {
    SyncOpts {
        resolve: vec![Resolution {
            rel: rel.into(),
            take,
        }],
        ..Default::default()
    }
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// The file named `name` anywhere under `dir`.
fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()? {
        let p = e.ok()?.path();
        if p.file_name().is_some_and(|n| n == name) && p.is_file() {
            return Some(p);
        }
        if p.is_dir()
            && let Some(f) = find(&p, name)
        {
            return Some(f);
        }
    }
    None
}

fn settled(bed: &Bed, home: &Home) {
    let p = plan_of(bed, home, true);
    assert_eq!(
        unit(&p, "work/r", UnitKind::Repo).decision,
        Decision::Nothing {
            why: "unchanged".into()
        },
        "the resolution is the new baseline"
    );
}

#[test]
fn a_conflict_both_sides_changed_is_resolvable_and_shows_their_dirt() {
    let (bed, home, ..) = conflicted();
    let p = plan_of(&bed, &home, true);
    let u = unit(&p, "work/r", UnitKind::Repo);
    assert!(
        matches!(&u.decision, Decision::Conflict { why } if why == "both changed since baseline"),
        "{:?}",
        u.decision
    );
    assert!(u.resolvable);
    assert_eq!((u.leaf_dirty, u.home_dirty), (1, 2));
    assert!(u.diff.is_none(), "a conflict is not diffed until answered");
}

#[test]
fn taking_this_copy_pushes_it_and_home_keeps_its_own_in_the_push_backup() {
    let (bed, home, h, l) = conflicted();
    let p = {
        let link = InProcess {
            home: &home,
            host: "leaf".into(),
            home_dir: bed.leaf_hd.clone(),
        };
        let cfg = bed.leaf_cfg();
        let state = bed.leaf_dirs.state();
        let answer = [Resolution {
            rel: "work/r".into(),
            take: Take::Leaf,
        }];
        let ctx = LeafCtx {
            resolve: &answer,
            ..ctx(&bed, &cfg, &state)
        };
        crate::plan::plan(&link, ctx, true, false).unwrap()
    };
    let u = unit(&p, "work/r", UnitKind::Repo);
    assert_eq!(u.decision, Decision::Resolved { take: Take::Leaf });
    assert!(u.refusals.is_empty(), "guardrail 1 steps aside: {u:#?}");
    assert!(u.diff.is_some());

    let r = sync_with(&bed, &home, &take("work/r", Take::Leaf)).unwrap();
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    let a = r.applied.iter().find(|a| a.rel == "work/r").unwrap();
    assert_eq!(a.action, "push (resolved)");
    assert!(a.push.is_some() && a.backup.is_none());
    assert_eq!(read(&h.join("README")), "the leaf's newer README\n");
    assert_eq!(
        read(&h.join("scratch.txt")),
        "the leaf's uncommitted work\n"
    );
    assert!(!h.join("default_1.profraw").exists());
    let kept = find(&bed.home_dirs.backup(), "default_1.profraw").expect("home's backup");
    assert_eq!(read(&kept), "coverage\n");
    assert!(l.join("scratch.txt").exists(), "the leaf is the source");
    settled(&bed, &home);
}

#[test]
fn taking_homes_copy_pulls_it_and_the_leaf_keeps_its_own_in_a_backup_here() {
    let (bed, home, h, l) = conflicted();
    let r = sync_with(&bed, &home, &take("work/r", Take::Home)).unwrap();
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    let a = r.applied.iter().find(|a| a.rel == "work/r").unwrap();
    assert_eq!(a.action, "pull (resolved)");
    let dir = a.backup.clone().expect("a backup on the leaf");
    assert!(
        dir.starts_with(bed.leaf_dirs.backup().join("resolved")) && dir.ends_with("work/r"),
        "{}",
        dir.display()
    );
    assert_eq!(read(&l.join("README")), "home's uncommitted README\n");
    assert_eq!(read(&l.join("default_1.profraw")), "coverage\n");
    assert!(!l.join("scratch.txt").exists());
    // Everything the pull replaced or deleted, as it was.
    assert_eq!(read(&dir.join("README")), "the leaf's newer README\n");
    assert_eq!(
        read(&dir.join("scratch.txt")),
        "the leaf's uncommitted work\n"
    );
    assert!(
        dir.join(".git").exists(),
        "the leaf's commit went with its refs"
    );
    assert!(h.join("default_1.profraw").exists(), "home is the source");
    let log = home.log(&Default::default()).unwrap();
    let line = log.iter().rev().find(|l| l.path == "work/r").unwrap();
    assert!(
        line.detail.contains(&format!("backup:{}", dir.display())),
        "{}",
        line.detail
    );
    assert!(
        crate::marker::store(&bed.leaf_dirs.state())
            .all::<Marker>()
            .unwrap()
            .is_empty()
    );
    settled(&bed, &home);
}

/// The resume carries on into the run the interrupted pull began: a backup
/// that is the only copy of what it replaced is never split, or skipped.
#[test]
fn an_interrupted_resolved_pull_resumes_into_the_same_backup() {
    let (bed, home, _, l) = conflicted();
    let inj = Inject::new(&bed, "work/r", How::Fails);
    let r = sync_inject(&bed, &home, &take("work/r", Take::Home), &inj).unwrap();
    assert!(
        matches!(&r.outcome, Outcome::Interrupted { rel, .. } if rel == "work/r"),
        "{:#?}",
        r.outcome
    );
    let markers: Vec<Marker> = crate::marker::store(&bed.leaf_dirs.state()).all().unwrap();
    let dir = markers[0].backup.clone().expect("the marker names the run");
    assert!(dir.is_dir());

    let p = plan_of(&bed, &home, true);
    assert_eq!(
        unit(&p, "work/r", UnitKind::Repo).decision,
        Decision::Resume {
            direction: Direction::Pull
        }
    );
    let r = yes(&bed, &home);
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    let a = r.applied.iter().find(|a| a.rel == "work/r").unwrap();
    assert!(a.resumed);
    assert_eq!(a.backup.as_ref(), Some(&dir));
    assert_eq!(
        read(&dir.join("scratch.txt")),
        "the leaf's uncommitted work\n"
    );
    assert!(!l.join("scratch.txt").exists());
    settled(&bed, &home);
}

#[test]
fn a_skipped_conflict_is_left_alone_and_the_rest_of_the_plan_goes() {
    let (bed, home, h, l) = conflicted();
    testutil::write(&bed.home_hd.join("work/notes.txt"), "pulls\n");
    let before = (testutil::fingerprint(&h), testutil::fingerprint(&l));
    let skip = SyncOpts {
        skip: vec!["work/r".into()],
        ..Default::default()
    };
    let r = sync_with(&bed, &home, &skip).unwrap();
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    assert!(r.applied.iter().all(|a| a.rel != "work/r"));
    assert_eq!(read(&bed.leaf_hd.join("work/notes.txt")), "pulls\n");
    assert_eq!(
        before,
        (testutil::fingerprint(&h), testutil::fingerprint(&l))
    );
    let p = plan_of(&bed, &home, true);
    let u = unit(&p, "work/r", UnitKind::Repo);
    assert!(
        matches!(u.decision, Decision::Conflict { .. }),
        "asked again"
    );
}

#[path = "apply_resolve_edge_tests.rs"]
mod edge;
