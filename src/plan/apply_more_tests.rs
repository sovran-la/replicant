//! Apply without a transfer (adoption, nothing to do), and deletions inside
//! a unit.

use super::*;

#[test]
fn identical_sides_are_adopted_as_the_baseline_without_a_transfer() {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/r"));
    let copy = std::process::Command::new("cp")
        .arg("-Rp")
        .arg(bed.home_hd.join("work/r"))
        .arg(bed.leaf_hd.join("work/r"))
        .status()
        .unwrap();
    assert!(copy.success());
    let home = bed.home();
    let prints = (
        testutil::fingerprint(&bed.home_hd),
        testutil::fingerprint(&bed.leaf_hd),
    );
    let r = yes(&bed, &home);
    assert_eq!(r.outcome, Outcome::Done);
    assert_eq!(
        prints,
        (
            testutil::fingerprint(&bed.home_hd),
            testutil::fingerprint(&bed.leaf_hd)
        ),
        "adoption writes no synced path"
    );
    assert_eq!(r.applied.len(), 1);
    assert_eq!(r.applied[0].action, "adopt");
    let b: Option<Baseline> = crate::baseline::store(&bed.leaf_dirs.state())
        .get("work/r")
        .unwrap();
    assert!(b.is_some());
    assert!(
        home.log(&Default::default())
            .unwrap()
            .iter()
            .any(|l| l.event == "adopt" && l.path == "work/r")
    );
    // Adopted once; the next sync has nothing to do.
    assert!(yes(&bed, &home).applied.is_empty());
}

#[test]
fn nothing_to_do_writes_nothing_and_says_so() {
    let bed = Bed::enrolled();
    let home = bed.home();
    let r = yes(&bed, &home);
    assert_eq!(r.outcome, Outcome::Done);
    assert!(r.applied.is_empty());
    assert_eq!(r.outcome.exit_code(), 0);
    assert!(
        crate::baseline::store(&bed.leaf_dirs.state())
            .all::<Baseline>()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn itemized_changes_include_deletions_on_the_target() {
    let bed = Bed::enrolled();
    tree(&bed.home_hd.join("work"));
    let home = bed.home();
    assert_eq!(yes(&bed, &home).outcome, Outcome::Done);
    // home loses a loose file and gains one; the pull mirrors both.
    std::fs::remove_file(bed.home_hd.join("work/data/blob.bin")).unwrap();
    testutil::write(&bed.home_hd.join("work/data/new.bin"), "new");
    let before = testutil::fingerprint(&bed.leaf_hd);
    let r = yes(&bed, &home);
    assert_eq!(r.outcome, Outcome::Done);
    let got = changed(&before, &testutil::fingerprint(&bed.leaf_hd));
    assert_eq!(
        got,
        BTreeSet::from(["work/data/blob.bin".into(), "work/data/new.bin".into()])
    );
    assert_eq!(got, itemized(&r));
    assert!(
        r.applied[0]
            .changes
            .contains(&"- data/blob.bin".to_string()),
        "{:?}",
        r.applied[0].changes
    );
}

#[test]
fn a_unit_gone_on_both_sides_drops_its_baseline() {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/r"));
    let home = bed.home();
    assert_eq!(yes(&bed, &home).outcome, Outcome::Done);
    std::fs::remove_dir_all(bed.home_hd.join("work/r")).unwrap();
    std::fs::remove_dir_all(bed.leaf_hd.join("work/r")).unwrap();
    let r = yes(&bed, &home);
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    let forgot: Vec<(&str, &str)> = r
        .applied
        .iter()
        .map(|a| (a.rel.as_str(), a.action.as_str()))
        .collect();
    assert_eq!(forgot, [("work/r", "forget")]);
    assert!(
        crate::baseline::store(&bed.leaf_dirs.state())
            .get::<Baseline>("work/r")
            .unwrap()
            .is_none()
    );
    assert!(
        leaf_baselines(&bed.home_dirs.state(), "leaf")
            .get::<Baseline>("work/r")
            .unwrap()
            .is_none()
    );
    assert!(yes(&bed, &home).applied.is_empty(), "and stays forgotten");
}
