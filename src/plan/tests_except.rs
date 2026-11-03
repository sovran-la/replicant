//! Review P1: a carve-out (`except`) is invisible to the table *and* to the transfer,
//! whether it names a directory or a single file.

use super::*;

/// `work` subscribed with `except` set to `carve`, then the leaf's loose unit
/// as both sides hold it now.
fn carved(carve: &str) -> Bed {
    let bed = Bed::enrolled();
    let text = std::fs::read_to_string(bed.home_dirs.fleet()).unwrap();
    std::fs::write(
        bed.home_dirs.fleet(),
        text.replace(
            "paths = [\"work\"]",
            &format!("paths = [\"work\"]\nexcept = [\"{carve}\"]"),
        ),
    )
    .unwrap();
    bed
}

fn loose_work(p: &Plan) -> &PlannedUnit {
    p.units
        .iter()
        .find(|u| u.rel == "work" && u.kind == UnitKind::Loose)
        .unwrap_or_else(|| panic!("no loose work: {:#?}", p.units))
}

/// Pull home's newer `work/notes.txt`; return the planned loose unit.
fn pull_with(bed: &Bed) -> PlannedUnit {
    testutil::write(&bed.home_hd.join("work/notes.txt"), "home's newer notes\n");
    testutil::write(&bed.leaf_hd.join("work/notes.txt"), "notes\n");
    let now = go(bed, &bed.home(), &bed.leaf_cfg(), false).unwrap();
    baselines(bed)
        .put(&Baseline {
            version: RECORD_VERSION,
            rel: "work".into(),
            kind: UnitKind::Loose,
            leaf: loose_work(&now).leaf.clone(),
            home: UnitState::Present("old".into()),
            at: 1,
        })
        .unwrap();
    let p = dry(bed);
    let u = loose_work(&p).clone();
    assert_eq!(u.decision, Decision::Pull, "{u:#?}");
    u
}

fn names(u: &PlannedUnit, what: &str) -> bool {
    let d = u.diff.as_ref().expect("a pull is diffed");
    d.changes.iter().any(|c| c.contains(what))
}

#[test]
fn a_file_except_present_only_on_the_leaf_is_never_in_a_pull() {
    let bed = carved("work/secret.txt");
    testutil::write(&bed.leaf_hd.join("work/secret.txt"), "leaf only\n");
    let u = pull_with(&bed);
    assert!(names(&u, "notes.txt"), "the pull itself: {:?}", u.diff);
    assert!(!names(&u, "secret.txt"), "carved out: {:?}", u.diff);
    assert_eq!(u.diff.as_ref().unwrap().deleted, 0, "{:?}", u.diff);
    assert!(bed.leaf_hd.join("work/secret.txt").exists());
}

#[test]
fn a_directory_except_present_only_on_the_leaf_is_never_in_a_pull() {
    let bed = carved("work/private");
    testutil::write(&bed.leaf_hd.join("work/private/key"), "leaf only\n");
    let u = pull_with(&bed);
    assert!(names(&u, "notes.txt"), "the pull itself: {:?}", u.diff);
    assert!(!names(&u, "private"), "carved out: {:?}", u.diff);
    assert_eq!(u.diff.as_ref().unwrap().deleted, 0, "{:?}", u.diff);
}

/// The carve-out is a name, not a prefix: `secret.txt.bak` still syncs.
#[test]
fn a_file_except_does_not_carve_out_its_namesakes() {
    let bed = carved("work/secret.txt");
    testutil::write(&bed.leaf_hd.join("work/secret.txt"), "leaf only\n");
    testutil::write(&bed.leaf_hd.join("work/secret.txt.bak"), "leaf only\n");
    let u = pull_with(&bed);
    assert!(
        names(&u, "- secret.txt.bak"),
        "not carved out: {:?}",
        u.diff
    );
    assert!(
        !u.diff
            .as_ref()
            .unwrap()
            .changes
            .contains(&"- secret.txt".into())
    );
}
