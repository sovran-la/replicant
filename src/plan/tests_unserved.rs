//! Review F5: a baseline home no longer serves is `Remove` only when the host
//! explicitly unsubscribed it and the leaf is unchanged since baseline. The
//! rest is weighted by home's reason (amended 2026-11-01): unsubscribed with
//! unpushed work is a blocking conflict; a cap narrowed or a path dropped by
//! hand is an orphan, shown and never blocking; a root renamed or mistyped
//! in fleet.toml blocks. None of them is ever a delete.
//! Every edit here lands behind a running `Home` (the F11 reload).

use super::*;
use crate::decide::UnservedReason;

/// `leaf` subscribed to `work/a` and `work/b`, a repo in each on both sides,
/// identical, plus a loose file in `work/b` on both.
fn two_subs() -> Bed {
    let bed = Bed::enrolled();
    for sub in ["work/a", "work/b"] {
        let h = testutil::repo(&bed.home_hd.join(sub).join("r"));
        copy(&h, &bed.leaf_hd.join(sub).join("r"));
    }
    testutil::write(&bed.home_hd.join("work/b/notes.txt"), "n\n");
    testutil::write(&bed.leaf_hd.join("work/b/notes.txt"), "n\n");
    let fleet = bed.home_dirs.fleet();
    let text = std::fs::read_to_string(&fleet)
        .unwrap()
        .replace("paths = [\"work\"]", "paths = [\"work/a\", \"work/b\"]");
    std::fs::write(&fleet, text).unwrap();
    bed
}

fn status(bed: &Bed, home: &Home) -> Plan {
    go(bed, home, &bed.leaf_cfg(), false).unwrap()
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

/// Baselines for every `work/b` unit, recording exactly what both sides hold
/// now: nothing unpushed, nothing to pull.
fn baseline_b_as_now(bed: &Bed, home: &Home) {
    let p = status(bed, home);
    for (rel, kind) in [("work/b/r", UnitKind::Repo), ("work/b", UnitKind::Loose)] {
        let u = p
            .units
            .iter()
            .find(|u| u.rel == rel && u.kind == kind)
            .unwrap();
        assert_eq!(
            u.decision,
            Decision::Nothing {
                why: "identical".into()
            },
            "{rel}"
        );
        put(bed, rel, kind, u.leaf.clone(), u.home.clone());
    }
}

fn why(u: &PlannedUnit) -> &str {
    match &u.decision {
        Decision::Conflict { why } => why,
        other => panic!("{}: expected a conflict row, got {other:?}", u.rel),
    }
}

fn unsubscribe(bed: &Bed, home: &Home, rel: &str) {
    let link = InProcess {
        home,
        host: "leaf".into(),
        home_dir: bed.leaf_hd.clone(),
    };
    let r = link
        .call(HomeRequest::Unsubscribe {
            paths: vec![bed.leaf_hd.join(rel).display().to_string()],
        })
        .unwrap();
    assert!(matches!(r, HomeReply::Unsubscribed(_)), "{r:?}");
}

/// Narrow the cap to `work/a` by hand, behind the running `home`, and move
/// `work/a/r` on home past its baseline, so the rest of the sync has a pull.
fn narrow_cap_and_move_a(bed: &Bed, home: &Home) {
    let now = status(bed, home);
    let a = unit(&now, "work/a/r");
    put(
        bed,
        "work/a/r",
        UnitKind::Repo,
        a.leaf.clone(),
        a.home.clone(),
    );
    testutil::hand_edit(
        &bed.home_dirs.fleet(),
        "[hosts.leaf]\n",
        "[hosts.leaf]\nallow = [\"work/a\"]\n",
    );
    let a = bed.home_hd.join("work/a/r");
    testutil::write(&a.join("README"), "home moved\n");
    testutil::git(&a, &["commit", "-qam", "home moved"]);
}

/// The plan as `status` renders it, with colour.
fn rendered(p: &Plan) -> String {
    crate::cli::render::plan(p, false, crate::cli::render::Style { color: true })
}

fn orphaned(reason: UnservedReason, changed: bool) -> Decision {
    Decision::Orphaned { reason, changed }
}

#[test]
fn a_cap_narrowed_by_hand_orphans_an_unchanged_leaf_and_the_sync_proceeds() {
    let bed = two_subs();
    let home = bed.home();
    baseline_b_as_now(&bed, &home);
    narrow_cap_and_move_a(&bed, &home);
    let p = status(&bed, &home);
    for rel in ["work/b/r", "work/b"] {
        assert_eq!(
            unit(&p, rel).decision,
            orphaned(UnservedReason::CapNarrowed, false),
            "{rel}"
        );
    }
    assert!(!p.blocked(), "an orphan does not block: exit 0");
    assert!(p.refused.is_empty(), "{:?}", p.refused);
    assert_eq!(
        p.orphaned.len(),
        1,
        "the subscription is shown, not refused"
    );
    assert_eq!(p.orphaned[0].unit, "work/b");

    let text = rendered(&p);
    let note = format!(
        "no longer served by home (cap narrowed); `replicant unsubscribe {}` to remove here, \
         or re-subscribe",
        bed.leaf_hd.join("work/b/r").display()
    );
    assert!(
        text.contains(&format!("\x1b[2m{note}\x1b[0m")),
        "dim: {text}"
    );
    assert!(
        text.contains("\x1b[2morphaned (cap narrowed)\x1b[0m"),
        "{text}"
    );

    // The rest of the sync proceeds: the dry run pulls work/a/r and does not
    // diff, let alone delete, the orphans.
    let d = go(&bed, &home, &bed.leaf_cfg(), true).unwrap();
    assert_eq!(unit(&d, "work/a/r").decision, Decision::Pull);
    assert!(unit(&d, "work/a/r").diff.as_ref().unwrap().files_changed > 0);
    assert!(unit(&d, "work/b/r").diff.is_none());
    assert!(!d.blocked());
    assert!(bed.leaf_hd.join("work/b/r/README").exists());
}

#[test]
fn a_cap_narrowed_over_unpushed_work_is_a_red_orphan_and_still_exit_0() {
    let bed = two_subs();
    let home = bed.home();
    baseline_b_as_now(&bed, &home);
    narrow_cap_and_move_a(&bed, &home);
    testutil::write(&bed.leaf_hd.join("work/b/r/README"), "unpushed\n");
    let p = status(&bed, &home);
    let r = unit(&p, "work/b/r");
    assert_eq!(r.decision, orphaned(UnservedReason::CapNarrowed, true));
    assert!(!p.blocked(), "warn, not block");
    let text = rendered(&p);
    assert!(
        text.contains(
            "changed here since baseline (1 file here) — home will not accept them\x1b[0m"
        ),
        "{text}"
    );
    assert!(
        text.contains("\x1b[31morphaned, changed (cap narrowed)\x1b[0m"),
        "red: {text}"
    );
    let d = go(&bed, &home, &bed.leaf_cfg(), true).unwrap();
    assert_eq!(unit(&d, "work/a/r").decision, Decision::Pull);
    assert!(unit(&d, "work/b/r").diff.is_none());
}

#[test]
fn a_root_renamed_in_fleet_toml_blocks() {
    let bed = two_subs();
    let home = bed.home();
    baseline_b_as_now(&bed, &home);
    std::fs::create_dir_all(bed.home_hd.join("work2")).unwrap();
    let typo = format!("path = \"{}/work2\"", bed.home_hd.display());
    let real = format!("path = \"{}/work\"", bed.home_hd.display());
    testutil::hand_edit(&bed.home_dirs.fleet(), &real, &typo);
    let p = status(&bed, &home);
    for rel in ["work/b/r", "work/b"] {
        let Decision::HomeRootMissing { why } = &unit(&p, rel).decision else {
            panic!("{rel}: {:?}", unit(&p, rel).decision)
        };
        assert_eq!(
            *why,
            format!(
                "home's root for {rel} is missing or renamed (subscription work/b: no root in \
                 fleet.toml covers it; roots: work2); fix fleet.toml on home"
            )
        );
    }
    assert!(p.blocked(), "exit 2");
    assert!(!p.refused.is_empty(), "the subscription is refused too");
    assert!(rendered(&p).contains("\x1b[31mhome's root for work/b/r is missing"));
}

#[test]
fn a_path_removed_from_subscriptions_by_hand_is_an_orphan_not_an_unsubscribe() {
    let bed = two_subs();
    let home = bed.home();
    baseline_b_as_now(&bed, &home);
    testutil::hand_edit(
        &bed.home_dirs.fleet(),
        "paths = [\"work/a\", \"work/b\"]",
        "paths = [\"work/a\"]",
    );
    let p = status(&bed, &home);
    assert_eq!(
        unit(&p, "work/b/r").decision,
        orphaned(UnservedReason::NotServed, false)
    );
    assert!(!p.blocked());
}

#[path = "tests_unserved_remove.rs"]
mod remove;

#[path = "tests_unserved_edge.rs"]
mod edge;
