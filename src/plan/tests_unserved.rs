//! Review F5: a baseline home no longer serves is `Remove` only when the host
//! explicitly unsubscribed it and the leaf is unchanged since baseline.
//! Everything else out of bounds — a cap narrowed by hand, a root renamed or
//! mistyped in fleet.toml — is a blocking conflict row, never a delete.
//! Every edit here lands behind a running `Home` (the F11 reload).

use super::*;

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

#[test]
fn a_cap_narrowed_by_hand_is_a_conflict_not_a_remove() {
    let bed = two_subs();
    let home = bed.home();
    baseline_b_as_now(&bed, &home);
    testutil::hand_edit(
        &bed.home_dirs.fleet(),
        "[hosts.leaf]\n",
        "[hosts.leaf]\nallow = [\"work/a\"]\n",
    );
    let p = status(&bed, &home);
    for rel in ["work/b/r", "work/b"] {
        let w = why(unit(&p, rel));
        assert!(
            w.starts_with(&format!("home no longer serves {rel} (cap narrowed);")),
            "{w}"
        );
        assert!(w.contains("unchanged since baseline"), "{w}");
        assert!(w.ends_with("re-subscribe, or unsubscribe to remove"), "{w}");
    }
    assert!(why(unit(&p, "work/b/r")).contains("; 1 file here,"));
    assert!(p.blocked(), "a conflict blocks: exit 2");

    // A dry run does not diff it either: there is nothing to carry out.
    let d = go(&bed, &home, &bed.leaf_cfg(), true).unwrap();
    assert!(unit(&d, "work/b/r").diff.is_none());
    assert!(bed.leaf_hd.join("work/b/r/README").exists());
}

#[test]
fn a_root_renamed_in_fleet_toml_is_a_conflict_not_a_remove() {
    let bed = two_subs();
    let home = bed.home();
    baseline_b_as_now(&bed, &home);
    std::fs::create_dir_all(bed.home_hd.join("work2")).unwrap();
    let typo = format!("path = \"{}/work2\"", bed.home_hd.display());
    let real = format!("path = \"{}/work\"", bed.home_hd.display());
    testutil::hand_edit(&bed.home_dirs.fleet(), &real, &typo);
    let p = status(&bed, &home);
    let w = why(unit(&p, "work/b/r"));
    assert!(w.contains("(no root serves it)"), "{w}");
    assert!(p.blocked());
}

#[test]
fn a_path_removed_from_subscriptions_by_hand_is_not_an_unsubscribe() {
    let bed = two_subs();
    let home = bed.home();
    baseline_b_as_now(&bed, &home);
    testutil::hand_edit(
        &bed.home_dirs.fleet(),
        "paths = [\"work/a\", \"work/b\"]",
        "paths = [\"work/a\"]",
    );
    let w = why(unit(&status(&bed, &home), "work/b/r")).to_string();
    assert!(w.contains("(not subscribed)"), "{w}");
}

#[test]
fn an_explicit_unsubscribe_with_an_unchanged_leaf_is_remove() {
    let bed = two_subs();
    let home = bed.home();
    baseline_b_as_now(&bed, &home);
    unsubscribe(&bed, &home, "work/b");
    let p = status(&bed, &home);
    assert_eq!(unit(&p, "work/b/r").decision, Decision::Remove);
    let loose = p
        .units
        .iter()
        .find(|u| u.rel == "work/b" && u.kind == UnitKind::Loose)
        .unwrap();
    assert_eq!(loose.decision, Decision::Remove);
    assert!(!p.blocked(), "{:#?}", p.units);

    // The dry run shows the deletion and still deletes nothing.
    let d = go(&bed, &home, &bed.leaf_cfg(), true).unwrap();
    assert!(unit(&d, "work/b/r").diff.as_ref().unwrap().deleted > 0);
    assert!(bed.leaf_hd.join("work/b/r/README").exists());
    let loose = loose_diff(&d, "work/b");
    assert!(loose.iter().any(|c| c == "- notes.txt"), "{loose:?}");
}

/// The loose unit's dry-run changes.
fn loose_diff(p: &Plan, rel: &str) -> Vec<String> {
    p.units
        .iter()
        .find(|u| u.rel == rel && u.kind == UnitKind::Loose)
        .and_then(|u| u.diff.as_ref())
        .unwrap_or_else(|| panic!("no loose diff for {rel}"))
        .changes
        .clone()
}

#[test]
fn removing_a_loose_unit_spares_a_repo_inside_it_that_is_in_conflict() {
    let bed = two_subs();
    let home = bed.home();
    baseline_b_as_now(&bed, &home);
    unsubscribe(&bed, &home, "work/b");
    testutil::write(&bed.leaf_hd.join("work/b/r/README"), "unpushed\n");
    let d = go(&bed, &home, &bed.leaf_cfg(), true).unwrap();
    assert!(matches!(
        unit(&d, "work/b/r").decision,
        Decision::Conflict { .. }
    ));
    let loose = loose_diff(&d, "work/b");
    assert!(
        !loose
            .iter()
            .any(|c| c[2..] == *"r" || c[2..].starts_with("r/")),
        "the loose removal would delete the repo: {loose:?}"
    );
    assert!(loose.iter().any(|c| c == "- notes.txt"), "{loose:?}");
}

#[test]
fn an_explicit_unsubscribe_with_unpushed_leaf_work_is_a_conflict() {
    let bed = two_subs();
    let home = bed.home();
    baseline_b_as_now(&bed, &home);
    unsubscribe(&bed, &home, "work/b");
    testutil::write(&bed.leaf_hd.join("work/b/r/README"), "unpushed\n");
    testutil::write(&bed.leaf_hd.join("work/b/new.txt"), "unpushed\n");
    let p = status(&bed, &home);
    for rel in ["work/b/r", "work/b"] {
        let w = why(unit(&p, rel));
        assert!(w.contains("(unsubscribed)"), "{w}");
        assert!(w.contains("changed since baseline"), "{w}");
        assert!(!w.contains("unchanged"), "{w}");
    }
    assert!(why(unit(&p, "work/b")).contains("; 2 files here,"));
}

#[test]
fn a_carve_out_by_unsubscribe_is_remove_and_resubscribing_restores_it() {
    let bed = two_subs();
    let home = bed.home();
    baseline_b_as_now(&bed, &home);
    unsubscribe(&bed, &home, "work/b/r");
    assert_eq!(
        unit(&status(&bed, &home), "work/b/r").decision,
        Decision::Remove
    );
    let link = InProcess {
        home: &home,
        host: "leaf".into(),
        home_dir: bed.leaf_hd.clone(),
    };
    link.call(HomeRequest::Subscribe {
        paths: vec![bed.leaf_hd.join("work/b/r").display().to_string()],
    })
    .unwrap();
    assert_eq!(
        unit(&status(&bed, &home), "work/b/r").decision,
        Decision::Nothing {
            why: "unchanged".into()
        }
    );
}

#[path = "tests_unserved_edge.rs"]
mod edge;
