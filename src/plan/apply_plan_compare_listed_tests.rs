//! The same list is not the same plan (the re-review's B1): a path the plan
//! already lists can change as well, and the fresh dry run then still reads
//! `~ path` or `- path`. What the unit would overwrite or delete must be what
//! the plan saw. The change arrives as the unit is announced.

use super::super::progress_tests::sync_watched_with;
use super::*;
use crate::daemon::wire::HomeRequest;

/// `change` runs as `unit` is announced, and the sync must stop there: failed
/// as "changed", `watched` as `change` left it, and `markers` markers on the
/// leaf afterwards.
fn stopped_at(
    bed: &Bed,
    home: &Home,
    opts: &SyncOpts,
    unit: &str,
    change: &(dyn Fn() + Sync),
    watched: &Path,
    markers: usize,
) {
    let before = std::sync::Mutex::new(None);
    let on = |e: &ProgressEvent, _: &crate::abort::Abort| {
        if let ProgressEvent::UnitStarted { rel, .. } = e
            && rel == unit
            && before.lock().unwrap().is_none()
        {
            testutil::next_second();
            change();
            *before.lock().unwrap() = Some(fingerprint(watched));
        }
    };
    let (r, _) = sync_watched_with(bed, home, opts, &on);
    match &r.outcome {
        Outcome::Failed { rel, why } => {
            assert_eq!(rel, unit);
            assert!(why.contains("changed since the plan"), "{why}");
        }
        o => panic!("not failed: {o:?}"),
    }
    assert_eq!(
        fingerprint(watched),
        before
            .into_inner()
            .unwrap()
            .expect("the unit was announced"),
        "nothing of it was written"
    );
    let m: Vec<Marker> = crate::marker::store(&bed.leaf_dirs.state()).all().unwrap();
    assert_eq!(m.len(), markers, "{m:?}");
}

/// A clean first sync of `tree`.
fn synced() -> (Bed, Home) {
    let bed = Bed::enrolled();
    tree(&bed.home_hd.join("work"));
    let home = bed.home();
    assert_eq!(yes(&bed, &home).outcome, Outcome::Done);
    testutil::next_second();
    (bed, home)
}

fn no_opts() -> SyncOpts {
    SyncOpts::default()
}

/// The review's g2: `~ notes.txt`, and the leaf edits `notes.txt`.
#[test]
fn a_pull_stops_at_a_leaf_edit_of_a_path_it_would_overwrite() {
    let (bed, home) = synced();
    testutil::write(&bed.home_hd.join("work/notes.txt"), "home edit\n");
    let leaf = bed.leaf_hd.join("work");
    let edit = || testutil::write(&leaf.join("notes.txt"), "LEAF EDIT AFTER THE PLAN\n");
    stopped_at(&bed, &home, &no_opts(), "work", &edit, &leaf, 0);
    assert_eq!(
        std::fs::read_to_string(leaf.join("notes.txt")).unwrap(),
        "LEAF EDIT AFTER THE PLAN\n"
    );
}

/// The review's g3: `- gone.txt`, and the leaf edits `gone.txt`.
#[test]
fn a_pull_stops_at_a_leaf_edit_of_a_path_it_would_delete() {
    let bed = Bed::enrolled();
    testutil::write(&bed.home_hd.join("work/gone.txt"), "x\n");
    testutil::write(&bed.home_hd.join("work/keep.txt"), "x\n");
    let home = bed.home();
    assert_eq!(yes(&bed, &home).outcome, Outcome::Done);
    testutil::next_second();
    std::fs::remove_file(bed.home_hd.join("work/gone.txt")).unwrap();
    let leaf = bed.leaf_hd.join("work");
    let edit = || testutil::write(&leaf.join("gone.txt"), "LEAF EDIT AFTER THE PLAN\n");
    stopped_at(&bed, &home, &no_opts(), "work", &edit, &leaf, 0);
    assert_eq!(
        std::fs::read_to_string(leaf.join("gone.txt")).unwrap(),
        "LEAF EDIT AFTER THE PLAN\n"
    );
}

/// The review's g4: `~ README` in a repo, and an uncommitted leaf edit of it.
#[test]
fn a_repo_pull_stops_at_an_uncommitted_edit_of_a_path_it_would_overwrite() {
    let (bed, home) = synced();
    let repo = bed.home_hd.join("work/alpha");
    testutil::write(&repo.join("README"), "home edit\n");
    testutil::git(&repo, &["commit", "-qam", "home"]);
    let leaf = bed.leaf_hd.join("work/alpha");
    let edit = || testutil::write(&leaf.join("README"), "UNCOMMITTED LEAF EDIT\n");
    stopped_at(&bed, &home, &no_opts(), "work/alpha", &edit, &leaf, 0);
    assert_eq!(
        std::fs::read_to_string(leaf.join("README")).unwrap(),
        "UNCOMMITTED LEAF EDIT\n"
    );
}

#[test]
fn a_confirmed_deletion_stops_at_an_edit_of_a_file_it_would_delete() {
    let (bed, home) = synced();
    std::fs::remove_dir_all(bed.home_hd.join("work/alpha")).unwrap();
    let leaf = bed.leaf_hd.join("work/alpha");
    let edit = || testutil::write(&leaf.join("README"), "LEAF EDIT AFTER THE PLAN\n");
    let opts = SyncOpts {
        confirm_delete: vec!["work/alpha".into()],
        ..Default::default()
    };
    stopped_at(&bed, &home, &opts, "work/alpha", &edit, &leaf, 0);
}

#[test]
fn a_removal_stops_at_an_edit_of_a_file_it_would_delete() {
    let (bed, home) = synced();
    let link = InProcess {
        home: &home,
        host: "leaf".into(),
        home_dir: bed.leaf_hd.clone(),
    };
    let leaf = bed.leaf_hd.join("work/alpha");
    link.call(HomeRequest::Unsubscribe {
        paths: vec![leaf.display().to_string()],
    })
    .unwrap();
    assert_eq!(
        unit(&plan_of(&bed, &home, true), "work/alpha", UnitKind::Repo).decision,
        Decision::Remove
    );
    let edit = || testutil::write(&leaf.join("README"), "LEAF EDIT AFTER THE PLAN\n");
    stopped_at(&bed, &home, &no_opts(), "work/alpha", &edit, &leaf, 0);
}

#[test]
fn a_pull_stops_at_a_link_repointed_that_it_would_replace() {
    let bed = Bed::enrolled();
    let (home_link, leaf) = (bed.home_hd.join("work/l"), bed.leaf_hd.join("work"));
    testutil::write(&bed.home_hd.join("work/keep.txt"), "x\n");
    std::os::unix::fs::symlink("first", &home_link).unwrap();
    let home = bed.home();
    assert_eq!(yes(&bed, &home).outcome, Outcome::Done);
    testutil::next_second();
    std::fs::remove_file(&home_link).unwrap();
    std::os::unix::fs::symlink("second", &home_link).unwrap();
    let repoint = || {
        std::fs::remove_file(leaf.join("l")).unwrap();
        std::os::unix::fs::symlink("leaf-only", leaf.join("l")).unwrap();
    };
    stopped_at(&bed, &home, &no_opts(), "work", &repoint, &leaf, 0);
    assert_eq!(
        std::fs::read_link(leaf.join("l")).unwrap(),
        Path::new("leaf-only")
    );
}

#[test]
fn a_single_file_pull_stops_at_a_leaf_edit_of_it() {
    let bed = Bed::enrolled();
    let text = std::fs::read_to_string(bed.home_dirs.fleet()).unwrap();
    std::fs::write(
        bed.home_dirs.fleet(),
        text.replace("paths = [\"work\"]", "paths = [\"work/one.txt\"]"),
    )
    .unwrap();
    testutil::write(&bed.home_hd.join("work/one.txt"), "first\n");
    let home = bed.home();
    assert_eq!(yes(&bed, &home).outcome, Outcome::Done);
    testutil::next_second();
    testutil::write(&bed.home_hd.join("work/one.txt"), "home edit\n");
    let file = bed.leaf_hd.join("work/one.txt");
    let edit = || testutil::write(&file, "LEAF EDIT AFTER THE PLAN\n");
    stopped_at(
        &bed,
        &home,
        &no_opts(),
        "work/one.txt",
        &edit,
        &bed.leaf_hd.join("work"),
        0,
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "LEAF EDIT AFTER THE PLAN\n"
    );
}

/// A resume keeps the marker it is resuming, as any stop there does.
#[test]
fn a_resumed_pull_stops_at_a_leaf_edit_and_keeps_its_marker() {
    let (bed, home) = super::super::marker_tests::interrupted("work", "/work/", "work/notes.txt");
    let leaf = bed.leaf_hd.join("work");
    let edit = || testutil::write(&leaf.join("notes.txt"), "LEAF EDIT AFTER THE PLAN\n");
    stopped_at(&bed, &home, &no_opts(), "work", &edit, &leaf, 1);
}

/// Either way: home's edit of what a push would replace stops it too, before
/// the push begins, though home would have kept a copy.
#[test]
fn a_push_stops_at_a_home_edit_of_a_path_it_would_replace() {
    let (bed, home) = synced();
    testutil::write(&bed.leaf_hd.join("work/notes.txt"), "pushed\n");
    let target = bed.home_hd.join("work");
    let edit = || testutil::write(&target.join("notes.txt"), "HOME EDIT AFTER THE PLAN\n");
    stopped_at(&bed, &home, &no_opts(), "work", &edit, &target, 0);
    assert!(home.pushes().unwrap().is_empty(), "no push begun");
}

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
