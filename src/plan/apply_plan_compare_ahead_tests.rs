//! What is no reason to stop: the comparison is of listed paths on the target,
//! not of everything that moved.

use super::super::progress_tests::sync_watched_with;
use super::listed::{no_opts, synced};
use super::*;

/// What `change` does as the unit is announced is no reason to stop.
fn goes_ahead(bed: &Bed, home: &Home, change: &(dyn Fn() + Sync)) {
    let on = |e: &ProgressEvent, _: &crate::abort::Abort| {
        if matches!(e, ProgressEvent::UnitStarted { rel, .. } if rel == "work") {
            testutil::next_second();
            change();
        }
    };
    let (r, _) = sync_watched_with(bed, home, &no_opts(), &on);
    assert_eq!(r.outcome, Outcome::Done);
}

/// A chmod loses no work, and the guard does not count it either.
#[test]
fn a_pull_goes_ahead_past_a_chmod_of_a_listed_path() {
    use std::os::unix::fs::PermissionsExt;
    let (bed, home) = synced();
    testutil::write(&bed.home_hd.join("work/notes.txt"), "home edit\n");
    let file = bed.leaf_hd.join("work/notes.txt");
    let chmod = || {
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    };
    goes_ahead(&bed, &home, &chmod);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "home edit\n");
}

/// Excluded paths are not the unit's, and nothing lists them.
#[test]
fn a_pull_goes_ahead_past_an_edit_of_a_path_the_unit_excludes() {
    let (bed, home) = synced();
    testutil::write(&bed.home_hd.join("work/notes.txt"), "home edit\n");
    let junk = bed.leaf_hd.join("work/target/junk");
    let edit = || testutil::write(&junk, "the leaf's own build output\n");
    goes_ahead(&bed, &home, &edit);
    assert_eq!(
        std::fs::read_to_string(&junk).unwrap(),
        "the leaf's own build output\n"
    );
    assert_eq!(
        std::fs::read_to_string(bed.leaf_hd.join("work/notes.txt")).unwrap(),
        "home edit\n"
    );
}

/// The check is of what would be lost: home's newer copy of a path a pull
/// lists loses nothing here, and is simply what the pull takes.
#[test]
fn a_pull_goes_ahead_past_a_newer_copy_on_home() {
    let (bed, home) = synced();
    let file = bed.home_hd.join("work/notes.txt");
    testutil::write(&file, "home edit\n");
    let edit = || testutil::write(&file, "home edit, again\n");
    goes_ahead(&bed, &home, &edit);
    assert_eq!(
        std::fs::read_to_string(bed.leaf_hd.join("work/notes.txt")).unwrap(),
        "home edit, again\n"
    );
}
