//! F5: what an explicit `unsubscribe` removes, and what it does not.

use super::*;

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
fn an_explicit_unsubscribe_with_unpushed_leaf_work_is_a_blocking_conflict() {
    let bed = two_subs();
    let home = bed.home();
    baseline_b_as_now(&bed, &home);
    unsubscribe(&bed, &home, "work/b");
    testutil::write(&bed.leaf_hd.join("work/b/r/README"), "unpushed\n");
    testutil::write(&bed.leaf_hd.join("work/b/new.txt"), "unpushed\n");
    let p = status(&bed, &home);
    for (rel, n) in [("work/b/r", "1 file"), ("work/b", "2 files")] {
        assert_eq!(
            why(unit(&p, rel)),
            format!(
                "unsubscribed but changed here since baseline ({n} here); push first, or \
                 `replicant unsubscribe --force {}`",
                bed.leaf_hd.join(rel).display()
            )
        );
    }
    assert!(p.blocked(), "exit 2");
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

/// Rev 7: a removal is a transfer from an empty source, and a
/// git-ignored file is never moved, so it is not deleted either.
#[test]
fn a_removal_spares_the_leafs_ignored_files() {
    let bed = two_subs();
    let home = bed.home();
    let h = bed.home_hd.join("work/b/r/.gitignore");
    testutil::write(&h, ".env*\n");
    copy(&h, &bed.leaf_hd.join("work/b/r/.gitignore"));
    baseline_b_as_now(&bed, &home);
    unsubscribe(&bed, &home, "work/b");
    testutil::write(&bed.leaf_hd.join("work/b/r/.env.local"), "SECRET\n");
    let d = go(&bed, &home, &bed.leaf_cfg(), true).unwrap();
    let r = unit(&d, "work/b/r");
    assert_eq!(r.decision, Decision::Remove, "ignored files are not state");
    let changes = &r.diff.as_ref().unwrap().changes;
    assert!(changes.iter().any(|c| c == "- README"), "{changes:?}");
    assert!(changes.iter().any(|c| c == "- .gitignore"), "{changes:?}");
    assert!(!changes.iter().any(|c| c.contains(".env")), "{changes:?}");
}
