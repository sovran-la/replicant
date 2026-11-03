//! A peer holding a pinned certificate, sending frames no honest sender would
//! (review M2): whatever order they come in, nothing lands outside the unit.
//! Each sequence ends in a refused frame and an untouched outside.

use super::*;
use std::os::unix::fs::PermissionsExt;

fn link(rel: &str, to: &Path) -> Entry {
    Entry {
        rel: rel.into(),
        kind: EntryKind::Symlink,
        size: 0,
        mtime_ns: 0,
        mode: 0o777,
        link: Some(to.display().to_string()),
    }
}

/// `leaf` drives home's side; home's result, which must be a refusal.
fn refused(s: &Served, leaf: impl FnOnce(&mut Conn)) -> String {
    let ok = || Ok(());
    push(s, &ok, leaf)
        .expect_err("the frame must be refused")
        .to_string()
}

fn empty(p: &Path) -> bool {
    std::fs::read_dir(p).unwrap().next().is_none()
}

/// p06: `a` was checked to be a directory; then it is a link.
#[test]
fn a_directory_replaced_by_a_link_is_not_written_through() {
    let t = tempfile::tempdir().unwrap();
    let (root, outside) = (t.path().join("unit"), t.path().join("outside"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let s = served(&root, None, &[]);
    let err = refused(&s, |c| {
        acked(c, Frame::json(Kind::Mkdirs, &[dir("a", 0o755)]).unwrap());
        acked(c, frame::batch(&[file("a/x", 1, 1)], b"x").unwrap());
        acked(c, Frame::json(Kind::Deletes, &[("a/x", false)]).unwrap());
        let r = [Replace {
            rel: "a".into(),
            kind: EntryKind::Dir,
        }];
        acked(c, Frame::json(Kind::Replace, &r).unwrap());
        acked(c, frame::batch(&[link("a", &outside)], b"").unwrap());
        let _ = c.send(&frame::batch(&[file("a/pwned", 1, 1)], b"p").unwrap());
    });
    assert!(err.contains("not a directory"), "{err}");
    assert!(empty(&outside), "nothing landed outside");
}

#[test]
fn a_link_is_not_written_through_after_a_file_beside_it_was_written_either() {
    let t = tempfile::tempdir().unwrap();
    let (root, outside) = (t.path().join("unit"), t.path().join("outside"));
    std::fs::create_dir_all(&outside).unwrap();
    crate::testutil::write(&root.join("a/first"), "x");
    let s = served(&root, None, &[]);
    let swap = root.join("a");
    let moved = t.path().join("moved");
    let err = refused(&s, |c| {
        acked(c, frame::batch(&[file("a/x", 1, 1)], b"x").unwrap());
        // Someone on this machine swaps the directory for a link meanwhile.
        std::fs::rename(&swap, &moved).unwrap();
        std::os::unix::fs::symlink(&outside, &swap).unwrap();
        let _ = c.send(&frame::batch(&[file("a/y", 1, 1)], b"y").unwrap());
    });
    assert!(err.contains("not a directory"), "{err}");
    assert!(empty(&outside));
}

/// p07: chmod follows a link; `DirModes` must not.
#[test]
fn final_modes_do_not_follow_a_link() {
    let t = tempfile::tempdir().unwrap();
    let (root, outside) = (t.path().join("unit"), t.path().join("outside"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("l")).unwrap();
    let s = served(&root, None, &[]);
    let err = refused(&s, |c| {
        let _ = c.send(&Frame::json(Kind::DirModes, &[dir("l", 0o777)]).unwrap());
    });
    assert!(err.contains("not a directory"), "{err}");
    let m = std::fs::metadata(&outside).unwrap().permissions().mode() & 0o7777;
    assert_eq!(m, 0o700, "the link's target keeps its mode");
}

#[test]
fn final_modes_of_a_missing_directory_are_an_error() {
    let t = tempfile::tempdir().unwrap();
    let s = served(t.path(), None, &[]);
    let err = refused(&s, |c| {
        let _ = c.send(&Frame::json(Kind::DirModes, &[dir("gone", 0o755)]).unwrap());
    });
    assert!(err.contains("gone"), "{err}");
}

/// p08: a link kept in the backup is never followed by a later keep.
#[test]
fn a_link_kept_in_the_backup_is_not_written_through() {
    let t = tempfile::tempdir().unwrap();
    let (root, outside) = (t.path().join("unit"), t.path().join("outside"));
    let run = t.path().join("backup/run-1");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("a")).unwrap();
    let s = served(&root, Some(run.clone()), &[]);
    let err = refused(&s, |c| {
        let r = [Replace {
            rel: "a".into(),
            kind: EntryKind::Symlink,
        }];
        acked(c, Frame::json(Kind::Replace, &r).unwrap());
        acked(c, Frame::json(Kind::Mkdirs, &[dir("a", 0o755)]).unwrap());
        acked(c, frame::batch(&[file("a/x", 1, 1)], b"1").unwrap());
        let _ = c.send(&frame::batch(&[file("a/x", 1, 2)], b"2").unwrap());
    });
    assert!(err.contains("not a directory"), "{err}");
    assert!(empty(&outside), "nothing renamed out through run/a");
    assert!(
        std::fs::symlink_metadata(run.join("a"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read(root.join("a/x")).unwrap(),
        b"1",
        "the file was not lost"
    );
}
