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

#[test]
fn a_runs_directory_notes_keep_each_directorys_first_line() {
    let t = tempfile::tempdir().unwrap();
    let run = t.path().join("backup/run-1");
    assert!(dirs_noted(&run).unwrap().is_empty(), "none noted");
    let b = Backup::new(run.clone());
    b.note_dir("a", Some(0o700)).unwrap();
    b.note_dir("new", None).unwrap();
    b.note_dir("a", Some(0o755)).unwrap();
    b.note_dir("new", Some(0o755)).unwrap();
    // A process that died mid-line.
    let log = run.with_extension("dirs");
    let mut text = fs::read_to_string(&log).unwrap();
    text.push_str("{\"p\":\"torn");
    fs::write(&log, text).unwrap();
    assert_eq!(
        dirs_noted(&run).unwrap(),
        [
            DirWas {
                rel: "a".into(),
                mode: Some(0o700)
            },
            DirWas {
                rel: "new".into(),
                mode: None
            },
        ]
    );
    // It is beside the run's directory, not in it: nothing to restore.
    assert!(!run.exists());
}

/// The review's m7: what is in the backup is whole, whatever stopped the copy.
#[test]
fn a_copy_across_filesystems_that_fails_leaves_nothing_that_looks_kept() {
    let t = tempfile::tempdir().unwrap();
    let unit = t.path().join("unit");
    write(&unit.join("tree/a-readable"), "x");
    write(&unit.join("tree/z-not"), "y");
    fs::set_permissions(unit.join("tree/z-not"), fs::Permissions::from_mode(0o000)).unwrap();
    let run = t.path().join("backup/run-1");
    let b = Backup::with_rename(run.clone(), exdev);
    let r = b.keep(&unit.join("tree"), "tree");
    fs::set_permissions(unit.join("tree/z-not"), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(r.is_err());
    assert!(
        fs::symlink_metadata(run.join("tree")).is_err(),
        "no partial copy"
    );
    let names: Vec<_> = fs::read_dir(&run)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(names.is_empty(), "{names:?}");
    assert!(unit.join("tree/a-readable").exists(), "the original stays");
}

#[test]
fn an_unfinished_copy_is_not_what_a_run_kept() {
    assert!(is_unfinished(".replicant-tmp-x"));
    assert!(is_unfinished("d/.replicant-tmp-x/inner"));
    assert!(!is_unfinished("d/x"));
}

#[test]
fn keeping_beside_leaves_the_path_and_links_or_copies_it() {
    let t = tempfile::tempdir().unwrap();
    let unit = t.path().join("unit");
    write(&unit.join("d/f"), "file");
    fs::set_permissions(unit.join("d/f"), fs::Permissions::from_mode(0o440)).unwrap();
    set_mtime(&unit.join("d/f"), 1_234, 567);
    std::os::unix::fs::symlink("f", unit.join("d/l")).unwrap();
    let before = Manifest::walk(&unit, &none(), false).unwrap();
    for (run, b) in [
        ("run-1", Backup::new(t.path().join("run-1"))),
        ("run-2", Backup::with_rename(t.path().join("run-2"), exdev)),
    ] {
        b.keep_beside(&unit.join("d/f"), "d/f").unwrap();
        b.keep_beside(&unit.join("d/l"), "d/l").unwrap();
        assert_eq!(
            Manifest::walk(&unit, &none(), false).unwrap(),
            before,
            "{run}"
        );
        let kept = Manifest::walk(&t.path().join(run), &none(), false).unwrap();
        let of = |m: &Manifest, r: &str| {
            let e = m.get(r).unwrap();
            (e.kind, e.size, e.mtime_ns, e.mode & 0o777, e.link.clone())
        };
        assert_eq!(of(&kept, "d/f"), of(&before, "d/f"), "{run}");
        assert_eq!(kept.get("d/l").unwrap().link.as_deref(), Some("f"), "{run}");
        // Once kept, the first copy stands: the path may have changed since.
        fs::remove_file(unit.join("d/f")).unwrap();
        write(&unit.join("d/f"), "changed");
        b.keep_beside(&unit.join("d/f"), "d/f").unwrap();
        assert_eq!(
            fs::read(t.path().join(run).join("d/f")).unwrap(),
            b"file",
            "{run}"
        );
        fs::remove_file(unit.join("d/f")).unwrap();
        write(&unit.join("d/f"), "file");
        fs::set_permissions(unit.join("d/f"), fs::Permissions::from_mode(0o440)).unwrap();
        set_mtime(&unit.join("d/f"), 1_234, 567);
    }
}
