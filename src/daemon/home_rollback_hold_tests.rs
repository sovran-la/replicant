//! Review A2: after `rollback` of a finished push, the pushing leaf's next
//! sync holds the unit as a conflict instead of pulling home's restored
//! copy over what it pushed, until it passes `--accept-rollback`. `--yes`
//! alone never accepts.

use super::*;
use crate::decide::Decision;
use crate::plan::apply::tests::{plan_of, sync_with, unit, yes};
use crate::plan::apply::{Outcome, SyncOpts};
use crate::testutil::{self, Bed};
use crate::units::UnitKind;

/// Home and leaf agree on `work`; the leaf edits a loose file and pushes;
/// home rolls that push back. Returns the push id.
fn rolled_back(bed: &Bed, home: &Home) -> String {
    testutil::repo(&bed.home_hd.join("work/alpha"));
    testutil::write(&bed.home_hd.join("work/notes.txt"), "home's\n");
    assert_eq!(yes(bed, home).outcome, Outcome::Done);
    testutil::write(&bed.leaf_hd.join("work/notes.txt"), "the leaf's, pushed\n");
    testutil::write(&bed.leaf_hd.join("work/new/deep.txt"), "a new tree\n");
    let r = yes(bed, home);
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    let id = r.applied[0].push.clone().expect("a push");
    assert_eq!(r.applied[0].rel, "work");
    home.rollback(&id).unwrap();
    assert_eq!(
        std::fs::read_to_string(bed.home_hd.join("work/notes.txt")).unwrap(),
        "home's\n",
        "home is back"
    );
    id
}

fn opts(accept: &[&str]) -> SyncOpts {
    SyncOpts {
        accept_rollback: accept.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

fn leaf_side(bed: &Bed) -> impl PartialEq + std::fmt::Debug {
    (
        testutil::fingerprint(&bed.leaf_hd),
        testutil::fingerprint(&bed.leaf_dirs.state()),
    )
}

#[test]
fn a_rolled_back_push_holds_the_pusher_until_it_accepts_then_pulls_then_settles() {
    let bed = Bed::enrolled();
    let home = bed.home();
    let id = rolled_back(&bed, &home);

    // Status and the dry run say why, and what to pass.
    let p = plan_of(&bed, &home, true);
    let Decision::Conflict { why } = &unit(&p, "work", UnitKind::Loose).decision else {
        panic!("{:#?}", p.units);
    };
    assert!(
        why.contains(&format!("home rolled back your push {id} at ")),
        "{why}"
    );
    assert!(why.contains("--accept-rollback work"), "{why}");
    assert!(
        why.contains(&bed.leaf_hd.join("work").display().to_string()),
        "{why}"
    );
    assert!(p.blocked());
    assert!(
        matches!(
            unit(&p, "work/alpha", UnitKind::Repo).decision,
            Decision::Nothing { .. }
        ),
        "another unit is not held"
    );

    // `--yes` alone: blocked, nothing written on the leaf.
    let before = leaf_side(&bed);
    let r = yes(&bed, &home);
    let Outcome::Blocked { why } = &r.outcome else {
        panic!("{:#?}", r.outcome);
    };
    assert!(
        why.iter().any(|w| w.contains("home rolled back your push")),
        "{why:?}"
    );
    assert_eq!(r.outcome.exit_code(), 2);
    assert!(r.applied.is_empty());
    assert_eq!(leaf_side(&bed), before, "the leaf is untouched");
    assert_eq!(
        std::fs::read_to_string(bed.leaf_hd.join("work/notes.txt")).unwrap(),
        "the leaf's, pushed\n"
    );
    // ...and still blocked the next time: the hold does not wear off.
    assert!(matches!(yes(&bed, &home).outcome, Outcome::Blocked { .. }));
    assert_eq!(leaf_side(&bed), before);

    // Accepted: the unit is decided by the table, and pulls home's copy.
    let accepted = plan_of_accepting(&bed, &home, &["work"]);
    assert_eq!(
        unit(&accepted, "work", UnitKind::Loose).decision,
        Decision::Pull
    );
    let r = sync_with(&bed, &home, &opts(&["work"]), crate::plan::RSYNC).unwrap();
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    assert_eq!(r.applied.len(), 1);
    assert_eq!(
        (r.applied[0].rel.as_str(), r.applied[0].action.as_str()),
        ("work", "pull")
    );
    assert_eq!(
        std::fs::read_to_string(bed.leaf_hd.join("work/notes.txt")).unwrap(),
        "home's\n"
    );
    assert!(!bed.leaf_hd.join("work/new").exists());

    // Settled on home: no longer reported, and the next sync has nothing to do.
    let push: Push = pushes(&bed.home_dirs.state()).get(&id).unwrap().unwrap();
    assert!(push.rollback_settled_at.is_some());
    assert!(home.rolled_back_for("leaf").unwrap().is_empty());
    let r = yes(&bed, &home);
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    assert!(r.applied.is_empty(), "{:#?}", r.applied);
    for u in &plan_of(&bed, &home, false).units {
        assert!(matches!(u.decision, Decision::Nothing { .. }), "{u:#?}");
    }
}

fn plan_of_accepting(bed: &Bed, home: &Home, accept: &[&str]) -> crate::plan::Plan {
    let accept: Vec<String> = accept.iter().map(|s| s.to_string()).collect();
    let link = testutil::InProcess {
        home,
        host: "leaf".into(),
        home_dir: bed.leaf_hd.clone(),
    };
    let cfg = bed.leaf_cfg();
    let dirs = (bed.leaf_dirs.state(), bed.leaf_dirs.scratch());
    let ctx = crate::plan::LeafCtx {
        accept_rollback: &accept,
        ..crate::plan::apply::tests::ctx(bed, &cfg, &dirs, crate::plan::RSYNC)
    };
    crate::plan::plan(&link, ctx, true, false).unwrap()
}

#[test]
fn accept_names_the_unit_by_its_local_path_too_and_must_name_one() {
    let bed = Bed::enrolled();
    let home = bed.home();
    rolled_back(&bed, &home);
    let before = leaf_side(&bed);
    // Naming nothing rolled back blocks, rather than being ignored.
    let r = sync_with(
        &bed,
        &home,
        &opts(&["work", "work/alpha"]),
        crate::plan::RSYNC,
    )
    .unwrap();
    let Outcome::Blocked { why } = &r.outcome else {
        panic!("{:#?}", r.outcome);
    };
    assert_eq!(
        why,
        &[
            "--accept-rollback work/alpha: home rolled back no push of this host's under that \
           path"
                .to_string()
        ]
    );
    assert_eq!(leaf_side(&bed), before);
    // `~/work/` is this leaf's path for the unit.
    let r = sync_with(&bed, &home, &opts(&["~/work/"]), crate::plan::RSYNC).unwrap();
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
}

#[test]
fn a_leaf_that_already_matches_home_is_adopted_and_settles_the_rollback() {
    let bed = Bed::enrolled();
    let home = bed.home();
    let id = rolled_back(&bed, &home);
    // The person copies home's loose files back by hand (mtimes and all:
    // the loose hash covers them): nothing is left for the hold to guard.
    let st = std::process::Command::new("rsync")
        .args(["-a", "--delete", "--exclude=/alpha"])
        .arg(format!("{}/", bed.home_hd.join("work").display()))
        .arg(format!("{}/", bed.leaf_hd.join("work").display()))
        .status()
        .unwrap();
    assert!(st.success());
    let p = plan_of(&bed, &home, false);
    assert_eq!(
        unit(&p, "work", UnitKind::Loose).leaf,
        unit(&p, "work", UnitKind::Loose).home
    );
    let r = yes(&bed, &home);
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    let push: Push = pushes(&bed.home_dirs.state()).get(&id).unwrap().unwrap();
    assert!(push.rollback_settled_at.is_some(), "{r:#?}");
}

#[test]
fn only_the_pushing_host_is_held() {
    let bed = Bed::enrolled();
    let home = bed.home();
    let id = rolled_back(&bed, &home);
    let mine = home.rolled_back_for("leaf").unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(
        (mine[0].id.as_str(), mine[0].unit.as_str()),
        (id.as_str(), "work")
    );
    assert!(home.rolled_back_for("other").unwrap().is_empty());
    // Another host settling the same unit settles nothing of this host's.
    home.settle_rollbacks("other", "work").unwrap();
    assert_eq!(home.rolled_back_for("leaf").unwrap().len(), 1);
    // Nor does this host agreeing on another unit.
    home.settle_rollbacks("leaf", "work/alpha").unwrap();
    assert_eq!(home.rolled_back_for("leaf").unwrap().len(), 1);
}

#[test]
fn the_sweep_keeps_an_unsettled_rollback_and_sweeps_it_once_settled() {
    let bed = Bed::enrolled();
    let home = bed.home();
    let id = rolled_back(&bed, &home);
    let later = crate::now_secs() + 30 * 86_400;
    assert_eq!(home.sweep_backups(later).unwrap(), 0);
    assert!(
        home.push_dir(&id).join("rollback").is_dir(),
        "the leaf's work is kept"
    );
    assert_eq!(
        home.rolled_back_for("leaf").unwrap().len(),
        1,
        "and still holds"
    );
    home.settle_rollbacks("leaf", "work").unwrap();
    assert_eq!(home.sweep_backups(later).unwrap(), 1);
    assert!(!home.push_dir(&id).exists());
}

#[path = "home_rollback_remove_tests.rs"]
mod remove;
