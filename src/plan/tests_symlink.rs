//! Review P4: a subscription base that is a symlink (`--as ~/work2` →
//! `/Volumes/data/work2`) is followed, and only the base is: links inside the
//! tree stay links. A dangling base is missing, so F4's rule applies.
//!
//! The "volume" here is a tempdir on the same disk; a real external volume
//! was not tested.

use super::*;
use crate::repos::Kind;
use crate::units::SubSpec;

/// Home's `work` with a repo, a loose file and a symlink inside the tree;
/// the leaf's `--as` target for `work` is `vol/work2`, holding a copy, and
/// `link` names the path the leaf config points at.
fn mounted(bed: &Bed, link: &Path) -> (LeafConfig, PathBuf) {
    let w = bed.home_hd.join("work");
    testutil::repo(&w.join("r"));
    testutil::write(&w.join("notes.txt"), "n\n");
    std::os::unix::fs::symlink("notes.txt", w.join("alias")).unwrap();
    let real = bed.tmp.path().join("vol/work2");
    copy(&w, &real);
    let mut cfg = bed.leaf_cfg();
    cfg.as_local.insert("work".into(), link.to_path_buf());
    (cfg, real)
}

fn spec() -> SubSpec {
    SubSpec {
        rel: "work".into(),
        exclude: vec!["target".into()],
        except: vec![],
        root: "work".into(),
    }
}

#[test]
fn a_symlinked_base_discovers_the_directory_it_points_at() {
    let bed = Bed::enrolled();
    let link = bed.tmp.path().join("work2");
    let (_, real) = mounted(&bed, &link);
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let via_link = Discovered::run(&[spec()], &|_| link.clone()).unwrap();
    let direct = Discovered::run(&[spec()], &|_| real.clone()).unwrap();
    assert_eq!(via_link.subs[0].2, direct.subs[0].2);
    assert_eq!(via_link.repos(), vec!["work/r"]);
    // Inside the tree a link is still a link, never followed.
    let alias = via_link.subs[0]
        .2
        .loose
        .iter()
        .find(|e| e.rel == "alias")
        .expect("alias");
    assert_eq!(alias.kind, Kind::Symlink);
    assert_eq!(alias.link.as_deref(), Some("notes.txt"));

    // And it hashes the same, unit by unit.
    let union = direct.repos();
    let a = units::snapshot(&via_link, &union, true).unwrap();
    let b = units::snapshot(&direct, &union, true).unwrap();
    assert_eq!(a, b);
}

#[test]
fn a_symlinked_as_base_plans_exactly_like_the_real_path() {
    let bed = Bed::enrolled();
    let link = bed.tmp.path().join("work2");
    let (cfg, real) = mounted(&bed, &link);
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let mut direct_cfg = cfg.clone();
    direct_cfg.as_local.insert("work".into(), real.clone());

    let home = bed.home();
    let p = go(&bed, &home, &cfg, true).unwrap();
    let q = go(&bed, &home, &direct_cfg, true).unwrap();
    let rows = |p: &Plan| -> Vec<(String, Decision, UnitState)> {
        p.units
            .iter()
            .map(|u| (u.rel.clone(), u.decision.clone(), u.leaf.clone()))
            .collect()
    };
    assert_eq!(rows(&p), rows(&q));
    for u in &p.units {
        assert_eq!(
            u.decision,
            Decision::Nothing {
                why: "identical".into()
            },
            "{}",
            u.rel
        );
    }
    assert!(
        std::fs::symlink_metadata(&link).unwrap().is_symlink(),
        "untouched"
    );
}

#[test]
fn a_dangling_base_is_missing_once_synced_and_a_first_pull_otherwise() {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/r"));
    let link = bed.tmp.path().join("work2");
    std::os::unix::fs::symlink(bed.tmp.path().join("vol/unmounted"), &link).unwrap();
    let mut cfg = bed.leaf_cfg();
    cfg.as_local.insert("work".into(), link.clone());

    // Never synced: the first pull, as for any missing base (F4).
    let p = go(&bed, &bed.home(), &cfg, false).unwrap();
    assert_eq!(unit(&p, "work/r").decision, Decision::Pull);

    // Synced once: the volume is gone, not the work.
    put_baseline(
        &bed,
        "work/r",
        UnitState::Present("l".into()),
        UnitState::Present("h".into()),
    );
    for dry in [false, true] {
        let err = go(&bed, &bed.home(), &cfg, dry).unwrap_err().to_string();
        assert!(
            err.contains("missing") && err.contains(&link.display().to_string()),
            "{err}"
        );
    }
}
