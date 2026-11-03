use super::*;
use crate::native::manifest::tests::set_mtime;
use crate::native::manifest::{EntryKind, Manifest};
use crate::testutil::write;

fn exdev(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::Error::from_raw_os_error(libc::EXDEV))
}

fn denied(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::Error::from_raw_os_error(libc::EACCES))
}

fn none() -> crate::exclude::Excludes {
    crate::exclude::Excludes::for_root(&[]).unwrap()
}

#[test]
fn across_filesystems_a_path_is_copied_with_its_metadata_then_removed() {
    let t = tempfile::tempdir().unwrap();
    let unit = t.path().join("unit");
    write(&unit.join("f"), "file");
    fs::set_permissions(unit.join("f"), fs::Permissions::from_mode(0o440)).unwrap();
    set_mtime(&unit.join("f"), 1_234, 567);
    write(&unit.join("tree/a/b"), "deep");
    set_mtime(&unit.join("tree/a/b"), 99, 1);
    fs::set_permissions(unit.join("tree/a"), fs::Permissions::from_mode(0o750)).unwrap();
    std::os::unix::fs::symlink("f", unit.join("l")).unwrap();
    let before = Manifest::walk(&unit, &none(), false).unwrap();

    let run = t.path().join("backup/run-1");
    let b = Backup::with_rename(run.clone(), exdev);
    for rel in ["f", "tree", "l"] {
        b.keep(&unit.join(rel), rel).unwrap();
        assert!(
            fs::symlink_metadata(unit.join(rel)).is_err(),
            "{rel} removed"
        );
    }
    let after = Manifest::walk(&run, &none(), false).unwrap();
    assert_eq!(
        after, before,
        "kept exactly: bytes' sizes, modes, mtimes, links"
    );
    assert_eq!(fs::read_to_string(run.join("tree/a/b")).unwrap(), "deep");
    assert_eq!(after.get("l").unwrap().kind, EntryKind::Symlink);
}

#[test]
fn any_other_rename_failure_keeps_nothing_and_says_why() {
    let t = tempfile::tempdir().unwrap();
    write(&t.path().join("f"), "x");
    let b = Backup::with_rename(t.path().join("run-1"), denied);
    let err = b.keep(&t.path().join("f"), "f").unwrap_err().to_string();
    assert!(err.contains("could not back up f"), "{err}");
    assert!(t.path().join("f").exists(), "the original stays");
}

#[test]
fn on_the_same_filesystem_a_path_is_renamed() {
    let t = tempfile::tempdir().unwrap();
    write(&t.path().join("u/f"), "x");
    let ino = |p: &Path| std::os::unix::fs::MetadataExt::ino(&fs::metadata(p).unwrap());
    let was = ino(&t.path().join("u/f"));
    let b = Backup::new(t.path().join("run-1"));
    b.keep(&t.path().join("u/f"), "deep/f").unwrap();
    assert_eq!(ino(&t.path().join("run-1/deep/f")), was);
    assert_eq!(b.dir(), t.path().join("run-1"));
}

#[test]
fn remove_tree_takes_files_links_and_trees_and_ignores_the_missing() {
    let t = tempfile::tempdir().unwrap();
    write(&t.path().join("d/e/f"), "x");
    std::os::unix::fs::symlink("d", t.path().join("l")).unwrap();
    remove_tree(&t.path().join("l")).unwrap();
    assert!(
        t.path().join("d/e/f").exists(),
        "a link goes, not its target"
    );
    remove_tree(&t.path().join("d")).unwrap();
    remove_tree(&t.path().join("nothing")).unwrap();
    assert_eq!(fs::read_dir(t.path()).unwrap().count(), 0);
}
