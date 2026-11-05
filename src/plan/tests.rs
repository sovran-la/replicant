//! A home and a leaf in one tempdir, wired in-process: every row of the table
//! end to end, guardrails against real dry runs, and proof nothing is written.

use super::*;
use crate::daemon::home::Home;
use crate::daemon::queue::EntryKind;
use crate::decide::Decision;
use crate::guard::Guard;
use crate::marker::Direction;
use crate::record::{RECORD_VERSION, Store};
use crate::state::UnitState;
use crate::testutil::{self, Bed, InProcess};
use crate::units::UnitKind;
use std::path::PathBuf;

fn go(bed: &Bed, home: &Home, cfg: &LeafConfig, dry: bool) -> Result<Plan> {
    let link = InProcess {
        home,
        host: "leaf".into(),
        home_dir: bed.leaf_hd.clone(),
    };
    let hd = cfg.home_dir().unwrap();
    let state = bed.leaf_dirs.state();
    plan(
        &link,
        LeafCtx {
            me: "leaf",
            home_dir: &hd,
            cfg,
            state_dir: &state,
            plane: Default::default(),
            accept_rollback: &[],
        },
        dry,
        false,
    )
}

fn dry(bed: &Bed) -> Plan {
    go(bed, &bed.home(), &bed.leaf_cfg(), true).unwrap()
}

fn unit<'a>(p: &'a Plan, rel: &str) -> &'a PlannedUnit {
    p.units
        .iter()
        .find(|u| u.rel == rel)
        .unwrap_or_else(|| panic!("no unit {rel}: {:#?}", p.units))
}

fn copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(to);
    assert!(
        std::process::Command::new("cp")
            .arg("-Rp")
            .arg(from)
            .arg(to)
            .status()
            .unwrap()
            .success()
    );
}

fn baselines(bed: &Bed) -> Store {
    crate::baseline::store(&bed.leaf_dirs.state())
}

fn put_baseline(bed: &Bed, rel: &str, leaf: UnitState, home: UnitState) {
    baselines(bed)
        .put(&Baseline {
            version: RECORD_VERSION,
            rel: rel.into(),
            kind: UnitKind::Repo,
            leaf,
            home,
            at: 1,
        })
        .unwrap();
}

#[test]
fn leaf_empty_pulls_everything_with_diffs_guard3_and_no_writes() {
    let bed = Bed::enrolled();
    testutil::tree(&bed.home_hd.join("work"));
    let before = (
        testutil::fingerprint(&bed.home_hd),
        testutil::fingerprint(&bed.leaf_hd),
    );
    let p = dry(&bed);
    assert_eq!(
        before,
        (
            testutil::fingerprint(&bed.home_hd),
            testutil::fingerprint(&bed.leaf_hd)
        ),
        "read-only"
    );
    assert!(
        baselines(&bed).all::<Baseline>().unwrap().is_empty(),
        "no baseline written"
    );

    for rel in ["work/alpha", "work/beta", "work/nested/gamma", "work"] {
        assert_eq!(unit(&p, rel).decision, Decision::Pull, "{rel}");
        assert!(
            unit(&p, rel).diff.as_ref().unwrap().files_changed > 0,
            "{rel}"
        );
    }
    let loose = unit(&p, "work").diff.as_ref().unwrap();
    assert!(
        loose.changes.iter().any(|c| c.ends_with(" notes.txt")),
        "{:?}",
        loose.changes
    );
    for banned in ["target", "alpha-wt", "alpha/", "beta/"] {
        assert!(
            !loose.changes.iter().any(|c| c[2..].starts_with(banned)),
            "{banned} in loose diff: {:?}",
            loose.changes
        );
    }
    let alpha = unit(&p, "work/alpha").diff.as_ref().unwrap();
    assert!(
        !alpha.changes.iter().any(|c| c.contains(".git/worktrees")),
        "parent's worktrees dir excluded"
    );
    // openrsync itemizes files bound for a missing directory as `>f.......`,
    // so "created" may read as "updated"; either way they travel.
    assert!(
        alpha.changes.iter().any(|c| c.ends_with(" .git/HEAD")),
        ".git travels: {:?}",
        alpha.changes
    );
    assert!(
        alpha.changes.iter().any(|c| c.ends_with(" .git/index")),
        "index is synced"
    );

    let beta = unit(&p, "work/beta");
    assert_eq!(beta.refusals.len(), 1);
    assert_eq!(beta.refusals[0].guard, Guard::SourceMidWrite);
    assert!(
        !unit(&p, "work/beta")
            .diff
            .as_ref()
            .unwrap()
            .changes
            .iter()
            .any(|c| c.contains("index.lock"))
    );
    assert!(p.blocked());
    assert_eq!(p.summary()[0].split(':').next().unwrap(), "pull ← home");
    assert!(p.summary()[0].contains("3 repos"), "{:?}", p.summary());
}

#[test]
fn home_empty_pushes_with_guard3_on_the_leaf_side() {
    let bed = Bed::enrolled();
    testutil::tree(&bed.leaf_hd.join("work"));
    let p = dry(&bed);
    assert_eq!(unit(&p, "work/alpha").decision, Decision::Push);
    assert_eq!(unit(&p, "work").decision, Decision::Push);
    assert_eq!(
        unit(&p, "work/beta").refusals[0].guard,
        Guard::SourceMidWrite
    );
    assert!(
        p.summary()
            .iter()
            .any(|l| l.starts_with("push → home: 3 repos")),
        "{:?}",
        p.summary()
    );
}

#[test]
fn identical_trees_without_baseline_are_nothing() {
    let bed = Bed::enrolled();
    testutil::tree(&bed.home_hd.join("work"));
    copy(&bed.home_hd.join("work"), &bed.leaf_hd.join("work"));
    let p = dry(&bed);
    for u in &p.units {
        assert_eq!(
            u.decision,
            Decision::Nothing {
                why: "identical".into()
            },
            "{}",
            u.rel
        );
        assert!(u.diff.is_none());
    }
    assert_eq!(p.summary(), vec!["nothing to do with home"]);
}

#[test]
fn both_populated_without_baseline_conflicts_and_is_not_diffed() {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/r"));
    let leaf_r = testutil::repo(&bed.leaf_hd.join("work/r"));
    testutil::git(
        &leaf_r,
        &["commit", "-q", "--allow-empty", "-m", "leaf only"],
    );
    let p = dry(&bed);
    let r = unit(&p, "work/r");
    assert!(
        matches!(r.decision, Decision::Conflict { .. }),
        "{:?}",
        r.decision
    );
    assert!(r.diff.is_none());
    assert!(p.blocked());
}

/// A phantom: the target's index says a file is modified, but the file on
/// disk is HEAD's (an index carried over from another machine; thistledown's
/// `tundra-tools`, 2026-11-05). No work is at risk, so guardrail 1 passes
/// and the push, which carries the source's index, goes through.
#[test]
fn a_stale_index_on_the_target_is_not_uncommitted_work() {
    let bed = Bed::enrolled();
    let (h, l) = diverged(&bed);
    // The leaf commits its work; home is at its own newer commit, with
    // README staged as something else and then restored on disk.
    testutil::git(&l, &["commit", "-qam", "leaf moved"]);
    testutil::write(&h.join("README"), "staged, never committed\n");
    testutil::git(&h, &["add", "README"]);
    testutil::write(&h.join("README"), "home's newer README\n");
    let status = go(&bed, &bed.home(), &bed.leaf_cfg(), false).unwrap();
    let home_now = unit(&status, "work/r").home.clone();
    put_baseline(&bed, "work/r", UnitState::Present("old".into()), home_now);
    let p = dry(&bed);
    let r = unit(&p, "work/r");
    assert_eq!(r.decision, Decision::Push);
    assert!(r.refusals.is_empty(), "{:?}", r.refusals);

    // The same, with the staged content left on disk: that is work.
    testutil::write(&h.join("README"), "staged, never committed\n");
    let status = go(&bed, &bed.home(), &bed.leaf_cfg(), false).unwrap();
    let home_now = unit(&status, "work/r").home.clone();
    put_baseline(&bed, "work/r", UnitState::Present("old".into()), home_now);
    let p = dry(&bed);
    let r = unit(&p, "work/r");
    assert_eq!(r.decision, Decision::Push);
    assert_eq!(r.refusals[0].guard, Guard::DirtyTarget);
    assert_eq!(r.refusals[0].detail, vec!["README"]);
}

/// A home repo, the leaf's copy of it, the leaf's uncommitted edit, and a
/// newer commit on home touching the same file.
fn diverged(bed: &Bed) -> (PathBuf, PathBuf) {
    let h = testutil::repo(&bed.home_hd.join("work/r"));
    let l = bed.leaf_hd.join("work/r");
    copy(&h, &l);
    testutil::write(&l.join("README"), "leaf's uncommitted work\n");
    testutil::write(&h.join("README"), "home's newer README\n");
    testutil::git(&h, &["commit", "-qam", "home moved"]);
    (h, l)
}

#[test]
fn baseline_rows_pull_push_nothing_and_guard1_on_pull() {
    let bed = Bed::enrolled();
    diverged(&bed);
    let status = go(&bed, &bed.home(), &bed.leaf_cfg(), false).unwrap();
    let (leaf_now, home_now) = (
        unit(&status, "work/r").leaf.clone(),
        unit(&status, "work/r").home.clone(),
    );
    assert!(
        unit(&status, "work/r").diff.is_none(),
        "status never runs a dry run"
    );

    // home changed, leaf same → pull, and the leaf's dirty README would die.
    put_baseline(
        &bed,
        "work/r",
        leaf_now.clone(),
        UnitState::Present("old".into()),
    );
    let p = dry(&bed);
    let r = unit(&p, "work/r");
    assert_eq!(r.decision, Decision::Pull);
    let g1 = r
        .refusals
        .iter()
        .find(|x| x.guard == Guard::DirtyTarget)
        .expect("guardrail 1");
    assert_eq!(g1.detail, vec!["README"]);

    // home same, leaf changed → push, and home is clean so guardrail 1 passes.
    put_baseline(
        &bed,
        "work/r",
        UnitState::Present("old".into()),
        home_now.clone(),
    );
    let r = dry(&bed);
    assert_eq!(unit(&r, "work/r").decision, Decision::Push);
    assert!(
        unit(&r, "work/r").refusals.is_empty(),
        "{:?}",
        unit(&r, "work/r").refusals
    );

    // both same → nothing.
    put_baseline(&bed, "work/r", leaf_now, home_now);
    assert_eq!(
        unit(&dry(&bed), "work/r").decision,
        Decision::Nothing {
            why: "unchanged".into()
        }
    );

    // both changed → conflict.
    put_baseline(
        &bed,
        "work/r",
        UnitState::Present("a".into()),
        UnitState::Present("b".into()),
    );
    assert!(matches!(
        unit(&dry(&bed), "work/r").decision,
        Decision::Conflict { .. }
    ));
}

#[path = "tests_more.rs"]
mod more;

#[path = "tests_deleted.rs"]
mod deleted;

#[path = "tests_except.rs"]
mod except;

#[path = "tests_ignored.rs"]
mod ignored;

#[path = "tests_unserved.rs"]
mod unserved;

#[path = "tests_rooted.rs"]
mod rooted;

#[path = "tests_symlink.rs"]
mod symlink;

#[path = "tests_lease_kill.rs"]
pub(crate) mod lease_kill;

#[path = "tests_inject.rs"]
pub(crate) mod inject;
