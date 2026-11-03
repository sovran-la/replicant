//! The receiver with a backup (home, on a push): what it replaces or
//! deletes is renamed into the push run's directory first.

use super::tests::{file, link, none};
use super::*;
use crate::native::manifest::Manifest;
use crate::native::manifest::tests::set_mtime;
use crate::testutil::write;

#[test]
fn with_a_backup_what_is_replaced_or_deleted_is_renamed_into_it() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("unit");
    write(&root.join("a"), "old a");
    write(&root.join("d/b"), "old b");
    set_mtime(&root.join("a"), 1_000, 5);
    std::os::unix::fs::symlink("a", root.join("l")).unwrap();
    let run = t.path().join("backup/leaf/T/run-1");
    let rx = Receiver::open(&root, false, None, Some(Backup::new(run.clone()))).unwrap();
    rx.file(&file("a", 5, 9, 0o644), b"new a").unwrap();
    rx.symlink(&link("l", "b")).unwrap();
    rx.delete(&[("d/b".into(), false), ("d".into(), true)])
        .unwrap();
    rx.file(&file("new", 1, 1, 0o644), b"n").unwrap();

    assert_eq!(fs::read_to_string(root.join("a")).unwrap(), "new a");
    assert_eq!(fs::read_to_string(run.join("a")).unwrap(), "old a");
    let kept = Manifest::walk(&run, &none(), false).unwrap();
    assert_eq!(
        kept.get("a").unwrap().mtime_ns,
        1_000_000_000_005,
        "renamed, not copied"
    );
    assert_eq!(fs::read_link(run.join("l")).unwrap(), Path::new("a"));
    assert_eq!(fs::read_to_string(run.join("d/b")).unwrap(), "old b");
    assert!(
        !run.join("new").exists(),
        "a created file has nothing to keep"
    );
    assert!(!root.join("d").exists());

    // Replaced again in the same run: the original stays the backup.
    rx.file(&file("a", 5, 10, 0o644), b"newer").unwrap();
    assert_eq!(fs::read_to_string(run.join("a")).unwrap(), "old a");
    assert_eq!(fs::read_to_string(root.join("a")).unwrap(), "newer");
}

#[test]
fn a_single_file_units_backup_is_kept_under_its_name() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("settings.json");
    write(&root, "old");
    let run = t.path().join("run-1");
    let rx = Receiver::open(&root, true, None, Some(Backup::new(run.clone()))).unwrap();
    rx.file(&file("", 3, 1, 0o644), b"new").unwrap();
    assert_eq!(
        fs::read_to_string(run.join("settings.json")).unwrap(),
        "old"
    );
}
