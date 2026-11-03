//! A receiver held to the target's manifest overwrites and deletes only what
//! the diff saw.

use super::*;
use crate::exclude::Excludes;
use crate::native::manifest::tests::set_mtime;
use crate::testutil::{fingerprint, write};

fn seen(root: &Path) -> Manifest {
    Manifest::walk(root, &Excludes::default(), false).unwrap()
}

fn entry(rel: &str, bytes: &[u8]) -> Entry {
    Entry {
        rel: rel.into(),
        kind: EntryKind::File,
        size: bytes.len() as u64,
        mtime_ns: 1_650_000_000_000_000_000,
        mode: 0o644,
        link: None,
    }
}

/// Everything under `root` is what it was, and the failure said so.
fn refused(root: &Path, before: &crate::plan::apply::tests::Print, r: Result<()>, why: &str) {
    let e = r.expect_err("the path moved since the diff").to_string();
    assert!(e.contains(why), "{e}");
    let mut now = fingerprint(root);
    now.retain(|k, _| !k.contains(".replicant-tmp-"));
    assert_eq!(&now, before, "nothing was touched");
}

#[test]
fn a_file_edited_since_the_diff_is_not_overwritten() {
    let t = tempfile::tempdir().unwrap();
    write(&t.path().join("a"), "as diffed");
    let target = seen(t.path());
    // Same size, another mtime: only the mtime tells.
    write(&t.path().join("a"), "leaf edit");
    set_mtime(&t.path().join("a"), 1_700_000_000, 5);
    let before = fingerprint(t.path());
    let rx = Receiver::open(t.path(), false, None, None)
        .unwrap()
        .expecting(&target);
    refused(
        t.path(),
        &before,
        rx.file(&entry("a", b"home"), b"home"),
        "changed",
    );
}

#[test]
fn a_file_grown_since_the_diff_is_not_overwritten() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("a");
    write(&p, "as diffed");
    set_mtime(&p, 1_700_000_000, 5);
    let target = seen(t.path());
    write(&p, "as diffed, and more");
    set_mtime(&p, 1_700_000_000, 5);
    let before = fingerprint(t.path());
    let rx = Receiver::open(t.path(), false, None, None)
        .unwrap()
        .expecting(&target);
    refused(
        t.path(),
        &before,
        rx.file(&entry("a", b"home"), b"home"),
        "changed",
    );
}

#[test]
fn a_file_created_since_the_diff_is_not_overwritten() {
    let t = tempfile::tempdir().unwrap();
    let target = seen(t.path());
    write(&t.path().join("new"), "leaf-only work");
    let before = fingerprint(t.path());
    let rx = Receiver::open(t.path(), false, None, None)
        .unwrap()
        .expecting(&target);
    refused(
        t.path(),
        &before,
        rx.file(&entry("new", b"home"), b"home"),
        "appeared",
    );
}

#[test]
fn a_link_retargeted_since_the_diff_is_not_overwritten() {
    let t = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink("one", t.path().join("l")).unwrap();
    let target = seen(t.path());
    std::fs::remove_file(t.path().join("l")).unwrap();
    std::os::unix::fs::symlink("two", t.path().join("l")).unwrap();
    let rx = Receiver::open(t.path(), false, None, None)
        .unwrap()
        .expecting(&target);
    let e = rx
        .file(&entry("l", b"home"), b"home")
        .unwrap_err()
        .to_string();
    assert!(e.contains("changed"), "{e}");
    assert_eq!(
        std::fs::read_link(t.path().join("l")).unwrap().to_str(),
        Some("two")
    );
}

#[test]
fn a_deletion_stops_at_a_file_edited_since_the_diff() {
    let t = tempfile::tempdir().unwrap();
    write(&t.path().join("a"), "as diffed");
    write(&t.path().join("b"), "as diffed");
    let target = seen(t.path());
    set_mtime(&t.path().join("b"), 1_700_000_000, 5);
    let before = fingerprint(t.path());
    let rx = Receiver::open(t.path(), false, None, None)
        .unwrap()
        .expecting(&target);
    // Children first: `b` is reached first, and nothing goes after it.
    let list = [("b".to_string(), false), ("a".to_string(), false)];
    refused(t.path(), &before, rx.delete(&list).map(|_| ()), "changed");
}

#[test]
fn a_kind_change_stops_at_a_path_edited_since_the_diff() {
    let t = tempfile::tempdir().unwrap();
    write(&t.path().join("a"), "as diffed");
    let target = seen(t.path());
    set_mtime(&t.path().join("a"), 1_700_000_000, 5);
    let before = fingerprint(t.path());
    let rx = Receiver::open(t.path(), false, None, None)
        .unwrap()
        .expecting(&target);
    refused(
        t.path(),
        &before,
        rx.replace("a", EntryKind::File, &[]),
        "changed",
    );
}

#[test]
fn a_directory_that_became_a_file_is_not_deleted() {
    let t = tempfile::tempdir().unwrap();
    std::fs::create_dir(t.path().join("d")).unwrap();
    let target = seen(t.path());
    std::fs::remove_dir(t.path().join("d")).unwrap();
    write(&t.path().join("d"), "leaf-only work");
    let before = fingerprint(t.path());
    let rx = Receiver::open(t.path(), false, None, None)
        .unwrap()
        .expecting(&target);
    refused(
        t.path(),
        &before,
        rx.delete(&[("d".to_string(), true)]).map(|_| ()),
        "changed",
    );
}

#[test]
fn what_the_diff_saw_is_overwritten_and_deleted_as_before() {
    let t = tempfile::tempdir().unwrap();
    write(&t.path().join("over"), "as diffed");
    write(&t.path().join("gone"), "as diffed");
    write(&t.path().join("d/x"), "as diffed");
    write(&t.path().join("kind"), "as diffed");
    let target = seen(t.path());
    // A chmod loses no work and is not a change here.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        t.path().join("over"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let rx = Receiver::open(t.path(), false, None, None)
        .unwrap()
        .expecting(&target);
    rx.file(&entry("over", b"home"), b"home").unwrap();
    rx.file(&entry("new", b"home"), b"home").unwrap();
    rx.replace("kind", EntryKind::File, &[]).unwrap();
    rx.file(&entry("kind", b"home"), b"home").unwrap();
    let list = [
        ("gone".to_string(), false),
        ("d/x".to_string(), false),
        ("d".to_string(), true),
    ];
    assert!(rx.delete(&list).unwrap().is_empty());
    // Already gone is fine, too: nothing is lost.
    assert!(
        rx.delete(&[("gone".to_string(), false)])
            .unwrap()
            .is_empty()
    );
    for f in ["over", "new", "kind"] {
        assert_eq!(std::fs::read(t.path().join(f)).unwrap(), b"home", "{f}");
    }
    assert!(!t.path().join("gone").exists() && !t.path().join("d").exists());
}

#[test]
fn a_receiver_held_to_nothing_writes_as_before() {
    let t = tempfile::tempdir().unwrap();
    write(&t.path().join("a"), "whatever is there");
    let rx = Receiver::open(t.path(), false, None, None).unwrap();
    rx.file(&entry("a", b"home"), b"home").unwrap();
    assert_eq!(std::fs::read(t.path().join("a")).unwrap(), b"home");
}

/// `dir` is on a filesystem that folds case (APFS by default; not Linux's).
fn folds_case(dir: &Path) -> bool {
    let p = dir.join("CaseProbe");
    write(&p, "x");
    let folds = dir.join("caseprobe").exists();
    std::fs::remove_file(p).unwrap();
    folds
}

#[test]
fn a_case_only_rename_is_the_file_the_diff_saw_and_no_new_work() {
    let t = tempfile::tempdir().unwrap();
    if !folds_case(t.path()) {
        return;
    }
    write(&t.path().join("d/FOO.txt"), "as diffed");
    let target = seen(t.path());
    let rx = Receiver::open(t.path(), false, None, None)
        .unwrap()
        .expecting(&target);
    rx.file(&entry("d/foo.txt", b"home"), b"home").unwrap();
    assert_eq!(std::fs::read(t.path().join("d/foo.txt")).unwrap(), b"home");
    // The old spelling's deletion would take what just arrived: it is stopped.
    let e = rx
        .delete(&[("d/FOO.txt".to_string(), false)])
        .unwrap_err()
        .to_string();
    assert!(e.contains("changed"), "{e}");
    assert_eq!(std::fs::read(t.path().join("d/foo.txt")).unwrap(), b"home");
}
