//! Review F4 end to end: a missing base is an error, never empty; a unit
//! that vanished since its baseline is "deleted on X", never a plain sync.

use super::*;
use crate::cli::render::{self, Style};

fn status(bed: &Bed) -> Plan {
    go(bed, &bed.home(), &bed.leaf_cfg(), false).unwrap()
}

fn present(s: &str) -> UnitState {
    UnitState::Present(s.into())
}

fn put(bed: &Bed, rel: &str, kind: UnitKind, leaf: UnitState, home: UnitState) {
    baselines(bed)
        .put(&Baseline {
            version: RECORD_VERSION,
            rel: rel.into(),
            kind,
            leaf,
            home,
            at: 1,
        })
        .unwrap();
}

/// Record what both sides hold now as `rel`'s baseline.
fn agree(bed: &Bed, rel: &str, kind: UnitKind) {
    let p = status(bed);
    let u = p.units.iter().find(|u| u.rel == rel && u.kind == kind);
    let u = u.unwrap_or_else(|| panic!("no {kind:?} {rel}: {:#?}", p.units));
    put(bed, rel, kind, u.leaf.clone(), u.home.clone());
}

fn trees(bed: &Bed) -> impl PartialEq + std::fmt::Debug {
    (
        testutil::fingerprint(&bed.home_hd),
        testutil::fingerprint(&bed.leaf_hd),
    )
}

#[test]
fn a_missing_leaf_base_that_was_synced_is_an_error_naming_path_and_host() {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/r"));
    let work2 = bed.tmp.path().join("work2");
    let mut cfg = bed.leaf_cfg();
    cfg.as_local.insert("work".into(), work2.clone());
    put(&bed, "work/r", UnitKind::Repo, present("l"), present("h"));
    let home = bed.home();
    for dry_run in [false, true] {
        let err = go(&bed, &home, &cfg, dry_run).unwrap_err().to_string();
        let want = format!(
            "subscription base {} does not exist on leaf",
            work2.display()
        );
        assert!(err.contains(&want), "{err}");
        assert!(err.contains("work/r was synced"), "{err}");
    }
    assert!(home.queue.view().is_empty(), "refused before queueing");
    assert!(!work2.exists());
}

#[test]
fn a_missing_leaf_base_never_synced_is_the_first_pull() {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/r"));
    let work2 = bed.tmp.path().join("work2");
    let mut cfg = bed.leaf_cfg();
    cfg.as_local.insert("work".into(), work2.clone());
    // A baseline elsewhere does not count: none lies under `work`.
    put(&bed, "other/x", UnitKind::Repo, present("l"), present("h"));
    let p = go(&bed, &bed.home(), &cfg, true).unwrap();
    assert_eq!(unit(&p, "work/r").decision, Decision::Pull);
}

/// A subscription below a root: `work/a`, which home does not have.
fn sub_below_root(bed: &Bed) {
    let text = std::fs::read_to_string(bed.home_dirs.fleet()).unwrap();
    std::fs::write(
        bed.home_dirs.fleet(),
        text.replace("paths = [\"work\"]", "paths = [\"work/a\"]"),
    )
    .unwrap();
    testutil::repo(&bed.leaf_hd.join("work/a/r"));
}

#[test]
fn a_missing_home_base_that_was_synced_is_an_error_naming_home() {
    let bed = Bed::enrolled();
    sub_below_root(&bed);
    put(&bed, "work/a/r", UnitKind::Repo, present("l"), present("h"));
    let home = bed.home();
    for dry_run in [false, true] {
        let err = go(&bed, &home, &bed.leaf_cfg(), dry_run)
            .unwrap_err()
            .to_string();
        let want = format!(
            "subscription base {} does not exist on home",
            bed.home_hd.join("work/a").display()
        );
        assert!(err.contains(&want), "{err}");
    }
}

#[test]
fn a_missing_home_base_never_synced_is_the_first_push() {
    let bed = Bed::enrolled();
    sub_below_root(&bed);
    let p = status(&bed);
    assert_eq!(unit(&p, "work/a/r").decision, Decision::Push);
}

#[test]
fn a_missing_home_root_is_an_error_from_home_even_on_a_first_sync() {
    let bed = Bed::enrolled();
    testutil::repo(&bed.leaf_hd.join("work/r"));
    std::fs::remove_dir_all(bed.home_hd.join("work")).unwrap();
    for dry_run in [false, true] {
        let err = go(&bed, &bed.home(), &bed.leaf_cfg(), dry_run)
            .unwrap_err()
            .to_string();
        let want = format!(
            "root work ({}) does not exist on home",
            bed.home_hd.join("work").display()
        );
        assert!(err.contains(&want), "{err}");
    }
}

#[test]
fn a_repo_vanished_on_the_leaf_is_deleted_on_the_leaf_and_never_pushed() {
    let bed = Bed::enrolled();
    let h = testutil::repo(&bed.home_hd.join("work/r"));
    copy(&h, &bed.leaf_hd.join("work/r"));
    agree(&bed, "work/r", UnitKind::Repo);
    std::fs::remove_dir_all(bed.leaf_hd.join("work/r")).unwrap();
    let note = "deleted on leaf — would remove 1 file on home; requires explicit confirmation";

    let s = status(&bed);
    let r = unit(&s, "work/r");
    assert_eq!(
        r.decision,
        Decision::DeletedOn {
            host: "leaf".into()
        }
    );
    assert_eq!(r.decision.direction(), None);
    assert_eq!(r.deletion_note("leaf", "home").as_deref(), Some(note));
    assert!(r.diff.is_none(), "status never runs a dry run");
    assert!(s.blocked());
    let text = render::plan(&s, false, Style { color: false });
    assert!(text.contains(note), "{text}");
    assert!(
        text.contains("deleted, needs confirmation: 1 units"),
        "{text}"
    );

    let before = trees(&bed);
    let p = dry(&bed);
    assert_eq!(before, trees(&bed), "read-only");
    let r = unit(&p, "work/r");
    assert!(matches!(r.decision, Decision::DeletedOn { .. }));
    let d = r.diff.as_ref().expect("the dry run shows what would go");
    assert!(d.deleted > 0, "{d:?}");
    assert!(d.changes.iter().any(|c| c == "- README"), "{d:?}");
    assert!(p.blocked());
    assert!(render::plan(&p, false, Style { color: false }).contains(note));
}

#[test]
fn a_repo_vanished_on_home_is_deleted_on_home_and_guards_the_leaf_s_work() {
    let bed = Bed::enrolled();
    let h = testutil::repo(&bed.home_hd.join("work/r"));
    let l = bed.leaf_hd.join("work/r");
    copy(&h, &l);
    testutil::write(&l.join("scratch.txt"), "untracked leaf work\n");
    agree(&bed, "work/r", UnitKind::Repo);
    std::fs::remove_dir_all(&h).unwrap();

    let s = status(&bed);
    let r = unit(&s, "work/r");
    assert_eq!(
        r.decision,
        Decision::DeletedOn {
            host: "home".into()
        }
    );
    assert_eq!(
        r.deletion_note("leaf", "home").as_deref(),
        Some("deleted on home — would remove 2 files on leaf; requires explicit confirmation"),
        "README tracked plus scratch.txt untracked"
    );

    let p = dry(&bed);
    let r = unit(&p, "work/r");
    assert!(r.diff.as_ref().unwrap().deleted > 0);
    let g1 = r.refusals.iter().find(|x| x.guard == Guard::DirtyTarget);
    assert_eq!(g1.expect("guardrail 1").detail, vec!["scratch.txt"]);
    assert!(l.join("scratch.txt").exists());
}

#[test]
fn loose_content_vanished_on_home_is_deleted_on_home() {
    let bed = Bed::enrolled();
    for side in [&bed.home_hd, &bed.leaf_hd] {
        testutil::write(&side.join("work/notes.txt"), "n\n");
        testutil::write(&side.join("work/data/blob.bin"), "b");
    }
    agree(&bed, "work", UnitKind::Loose);
    std::fs::remove_file(bed.home_hd.join("work/notes.txt")).unwrap();
    std::fs::remove_dir_all(bed.home_hd.join("work/data")).unwrap();

    let s = status(&bed);
    let w = unit(&s, "work");
    assert_eq!(w.kind, UnitKind::Loose);
    assert_eq!(
        w.decision,
        Decision::DeletedOn {
            host: "home".into()
        }
    );
    assert_eq!(
        w.deletion_note("leaf", "home").as_deref(),
        Some("deleted on home — would remove 2 files on leaf; requires explicit confirmation")
    );
}

/// The rows next to the new one are untouched end to end: one side changed
/// but did not vanish still pulls.
#[test]
fn a_unit_that_changed_but_did_not_vanish_still_pulls() {
    let bed = Bed::enrolled();
    let h = testutil::repo(&bed.home_hd.join("work/r"));
    copy(&h, &bed.leaf_hd.join("work/r"));
    agree(&bed, "work/r", UnitKind::Repo);
    testutil::git(&h, &["commit", "-q", "--allow-empty", "-m", "moved"]);
    assert_eq!(unit(&status(&bed), "work/r").decision, Decision::Pull);
}
