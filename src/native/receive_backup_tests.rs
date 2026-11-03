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

#[test]
fn every_directory_a_run_changes_is_noted_before_it_does() {
    use crate::native::backup::{DirWas, dirs_noted};
    use crate::native::manifest::{Entry, EntryKind};
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let (root, run) = (t.path().join("unit"), t.path().join("backup/run-1"));
    let dir = |rel: &str, mode| Entry {
        rel: rel.into(),
        kind: EntryKind::Dir,
        size: 0,
        mtime_ns: 0,
        mode,
        link: None,
    };
    let chmod = |p: &str, m| {
        std::fs::set_permissions(root.join(p), std::fs::Permissions::from_mode(m)).unwrap()
    };
    write(&root.join("gone/x"), "x");
    chmod("gone", 0o700);
    std::fs::create_dir(root.join("empties")).unwrap();
    chmod("empties", 0o750);
    std::fs::create_dir(root.join("opened")).unwrap();
    chmod("opened", 0o555);
    std::fs::create_dir(root.join("same")).unwrap();
    chmod("same", 0o755);
    write(&root.join("holds/.env"), "kept");
    chmod("holds", 0o711);
    let rx = Receiver::open(&root, false, None, Some(Backup::new(run.clone()))).unwrap();
    rx.delete(&[("gone/x".to_string(), false), ("gone".to_string(), true)])
        .unwrap();
    rx.replace("empties", EntryKind::Dir, &[]).unwrap();
    rx.mkdir(&dir("opened", 0o755)).unwrap();
    rx.mkdir(&dir("made", 0o500)).unwrap();
    rx.dir_modes(&[dir("same", 0o755), dir("made", 0o500), dir("opened", 0o555)])
        .unwrap();
    let kept = rx.delete(&[("holds".to_string(), true)]).unwrap();
    assert_eq!(
        kept,
        ["holds"],
        "still holding a path the unit does not sync"
    );
    chmod("made", 0o755);
    let m = |rel: &str, mode| DirWas {
        rel: rel.into(),
        mode,
    };
    assert_eq!(
        dirs_noted(&run).unwrap(),
        [
            m("gone", Some(0o700)),
            m("empties", Some(0o750)),
            m("opened", Some(0o555)),
            m("made", None),
            m("holds", Some(0o711)),
        ],
        "the first line of each; `same` did not change"
    );
}

#[test]
fn a_leaf_keeps_no_notes() {
    let t = tempfile::tempdir().unwrap();
    write(&t.path().join("unit/d/x"), "x");
    let rx = Receiver::open(&t.path().join("unit"), false, None, None).unwrap();
    rx.delete(&[("d/x".to_string(), false), ("d".to_string(), true)])
        .unwrap();
    assert_eq!(
        std::fs::read_dir(t.path()).unwrap().count(),
        1,
        "just the unit"
    );
}
