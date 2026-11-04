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

/// The re-review's m10, end to end: this used to fail every run, and stop the
/// units after it.
#[test]
fn a_changed_file_in_a_read_only_directory_does_not_stop_a_sync() {
    use std::os::unix::fs::PermissionsExt;
    let chmod = |p: &Path, mode: u32| {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    let bed = Bed::enrolled();
    let (h, l) = (bed.home_hd.join("work"), bed.leaf_hd.join("work"));
    testutil::write(&h.join("ro/f.txt"), "old\n");
    testutil::repo(&h.join("zeta"));
    chmod(&h.join("ro"), 0o555);
    let home = bed.home();
    assert_eq!(yes(&bed, &home).outcome, Outcome::Done);
    testutil::next_second();
    chmod(&h.join("ro"), 0o755);
    testutil::write(&h.join("ro/f.txt"), "newer contents\n");
    chmod(&h.join("ro"), 0o555);
    testutil::write(&h.join("zeta/README"), "home edit\n");
    testutil::git(&h.join("zeta"), &["commit", "-qam", "home"]);
    let r = yes(&bed, &home);
    let mode = std::fs::metadata(l.join("ro"))
        .unwrap()
        .permissions()
        .mode()
        & 0o7777;
    let (text, later) = (
        std::fs::read_to_string(l.join("ro/f.txt")),
        std::fs::read_to_string(l.join("zeta/README")),
    );
    let again = plan_of(&bed, &home, true);
    chmod(&h.join("ro"), 0o755);
    chmod(&l.join("ro"), 0o755);
    assert_eq!(r.outcome, Outcome::Done);
    assert_eq!(text.unwrap(), "newer contents\n");
    assert_eq!(later.unwrap(), "home edit\n", "the unit after it went too");
    assert_eq!(mode, 0o555);
    assert!(
        again
            .units
            .iter()
            .all(|u| matches!(u.decision, Decision::Nothing { .. })),
        "nothing left: {:#?}",
        again.units
    );
}
