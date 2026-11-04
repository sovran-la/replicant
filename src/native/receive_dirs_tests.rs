//! Directories that get in the way, and what a failure on the tree says.

use super::*;
use std::io;
use std::os::unix::fs::PermissionsExt;

/// The review's m1 (p15): a read-only target directory whose source is
/// writable is filled, not refused forever.
#[test]
fn an_existing_read_only_directory_is_opened_to_be_filled() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path();
    std::fs::create_dir(root.join("ro")).unwrap();
    fs::set_permissions(root.join("ro"), fs::Permissions::from_mode(0o555)).unwrap();
    let rx = Receiver::open(root, false, None, None).unwrap();
    let e = Entry {
        rel: "ro".into(),
        kind: EntryKind::Dir,
        size: 0,
        mtime_ns: 0,
        mode: 0o755,
        link: None,
    };
    rx.mkdir(&e).unwrap();
    let f = Entry {
        rel: "ro/f".into(),
        kind: EntryKind::File,
        size: 1,
        mtime_ns: 1,
        mode: 0o644,
        link: None,
    };
    rx.file(&f, b"x").unwrap();
    rx.dir_modes(std::slice::from_ref(&e)).unwrap();
    let m = fs::metadata(root.join("ro")).unwrap();
    assert_eq!(m.permissions().mode() & 0o7777, 0o755);
    assert_eq!(fs::read(root.join("ro/f")).unwrap(), b"x");
}

/// The review's m2: "Permission denied", on what could be a 70k-file unit,
/// says nothing. Every failure on the tree says which path.
#[test]
fn a_failed_write_names_the_path() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path();
    crate::testutil::write(&root.join("ro/old"), "x");
    crate::testutil::write(&root.join("ro/.replicant-tmp-dead"), "x");
    fs::set_permissions(root.join("ro"), fs::Permissions::from_mode(0o555)).unwrap();
    let rx = Receiver::open(root, false, None, None).unwrap();
    let file = Entry {
        rel: "ro/new".into(),
        kind: EntryKind::File,
        size: 1,
        mtime_ns: 1,
        mode: 0o644,
        link: None,
    };
    let dir = Entry {
        rel: "ro/sub".into(),
        kind: EntryKind::Dir,
        mode: 0o755,
        size: 0,
        mtime_ns: 0,
        link: None,
    };
    let link = Entry {
        rel: "ro/l".into(),
        kind: EntryKind::Symlink,
        link: Some("x".into()),
        mode: 0o777,
        size: 0,
        mtime_ns: 0,
    };
    let errs = [
        ("ro/new", rx.file(&file, b"x").unwrap_err()),
        ("ro/sub", rx.mkdir(&dir).unwrap_err()),
        ("ro/l", rx.symlink(&link).unwrap_err()),
        (
            "ro/old",
            rx.delete(&[("ro/old".to_string(), false)]).unwrap_err(),
        ),
        (
            "ro/.replicant-tmp-dead",
            rx.clean(&["ro/.replicant-tmp-dead".to_string()])
                .unwrap_err(),
        ),
    ];
    fs::set_permissions(root.join("ro"), fs::Permissions::from_mode(0o755)).unwrap();
    for (rel, e) in errs {
        let e = e.to_string();
        assert!(e.contains(rel), "{rel}: {e}");
        assert!(e.contains("denied"), "{rel}: {e}");
    }
}

/// The review's m5. What `sync_dirs` does with an fsync that fails, and with
/// what it may pass over.
mod durable {
    use super::*;
    use std::path::PathBuf;

    fn fails(_: &fs::File) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::EIO))
    }

    fn not_supported(_: &fs::File) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::EINVAL))
    }

    #[test]
    fn an_fsync_that_fails_is_the_transfers_failure() {
        let t = tempfile::tempdir().unwrap();
        let e = sync_each(&[t.path().to_path_buf()], fails)
            .unwrap_err()
            .to_string();
        assert!(e.contains("could not be synced"), "{e}");
        sync_each(&[t.path().to_path_buf()], not_supported).unwrap();
    }

    #[test]
    fn a_directory_since_removed_or_closed_to_this_user_is_passed_over() {
        let t = tempfile::tempdir().unwrap();
        let shut = t.path().join("shut");
        std::fs::create_dir(&shut).unwrap();
        fs::set_permissions(&shut, fs::Permissions::from_mode(0o300)).unwrap();
        crate::testutil::write(&t.path().join("file"), "x");
        let dirs = [
            t.path().join("gone"),
            t.path().join("file/under"),
            shut.clone(),
        ];
        let r = sync_each(&dirs, fails);
        fs::set_permissions(&shut, fs::Permissions::from_mode(0o755)).unwrap();
        r.unwrap();
    }

    #[test]
    fn a_kind_changes_removal_is_made_durable_too() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        crate::testutil::write(&root.join("f/was-file"), "x");
        std::fs::create_dir_all(root.join("d/was-dir")).unwrap();
        let rx = Receiver::open(root, false, None, None).unwrap();
        rx.replace("f/was-file", EntryKind::File, &[]).unwrap();
        rx.replace("d/was-dir", EntryKind::Dir, &[]).unwrap();
        let touched = rx.touched.lock().unwrap();
        let want: [PathBuf; 2] = [root.join("f"), root.join("d")];
        for d in want {
            assert!(touched.contains(&d), "{d:?}: {touched:?}");
        }
    }
}
