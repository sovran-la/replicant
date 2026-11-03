use super::*;
use crate::native::manifest::Manifest;
use crate::testutil::write;
use std::os::unix::fs::PermissionsExt;

pub(super) fn file(rel: &str, size: u64, mtime_ns: i64, mode: u32) -> Entry {
    Entry {
        rel: rel.into(),
        kind: EntryKind::File,
        size,
        mtime_ns,
        mode,
        link: None,
    }
}
fn dir(rel: &str, mode: u32) -> Entry {
    Entry {
        rel: rel.into(),
        kind: EntryKind::Dir,
        size: 0,
        mtime_ns: 0,
        mode,
        link: None,
    }
}
pub(super) fn link(rel: &str, to: &str) -> Entry {
    Entry {
        rel: rel.into(),
        kind: EntryKind::Symlink,
        size: 0,
        mtime_ns: 0,
        mode: 0o777,
        link: Some(to.into()),
    }
}
pub(super) fn none() -> Excludes {
    Excludes::for_root(&[]).unwrap()
}
fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn a_file_lands_whole_with_the_sources_mode_and_mtime() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("unit");
    let rx = Receiver::open(&root, false, None, None).unwrap();
    rx.mkdir(&dir("d", 0o755)).unwrap();
    let e = file("d/a.txt", 5, 1_600_000_000_987_654_321, 0o640);
    rx.file(&e, b"hello").unwrap();
    let ro = file("d/ro", 2, 1_000, 0o444);
    rx.file(&ro, b"ro").unwrap();
    rx.dir_modes(&[dir("d", 0o755)]).unwrap();
    rx.sync_dirs().unwrap();

    assert_eq!(fs::read(root.join("d/a.txt")).unwrap(), b"hello");
    let m = Manifest::walk(&root, &none(), false).unwrap();
    assert_eq!(
        m.get("d/a.txt"),
        Some(&e),
        "size, mtime_ns and mode as sent"
    );
    assert_eq!(m.get("d/ro"), Some(&ro));
    assert_eq!(names(&root.join("d")), ["a.txt", "ro"], "no temporary left");
}

#[test]
fn a_file_that_does_not_arrive_whole_never_replaces_its_target() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().to_path_buf();
    write(&root.join("a"), "old");
    let rx = Receiver::open(&root, false, None, None).unwrap();

    let mut short = rx.begin(&file("a", 10, 1, 0o644)).unwrap();
    short.write(b"new").unwrap();
    let err = short.finish().unwrap_err().to_string();
    assert!(err.contains("3 of its 10 bytes"), "{err}");

    let mut long = rx.begin(&file("a", 2, 1, 0o644)).unwrap();
    assert!(long.write(b"too long").is_err());
    drop(long);

    let mut dropped = rx.begin(&file("a", 3, 1, 0o644)).unwrap();
    dropped.write(b"ne").unwrap();
    drop(dropped);

    assert_eq!(fs::read_to_string(root.join("a")).unwrap(), "old");
    assert_eq!(names(&root), ["a"]);
}

#[test]
fn a_stale_temporary_is_replaced_and_clean_removes_only_temporaries() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().to_path_buf();
    write(&root.join(".replicant-tmp-a"), "dead run");
    write(&root.join("sub/.replicant-tmp-b"), "dead run");
    write(&root.join("keep"), "x");
    let rx = Receiver::open(&root, false, None, None).unwrap();
    rx.file(&file("a", 1, 1, 0o644), b"x").unwrap();
    rx.clean(&[
        "sub/.replicant-tmp-b".into(),
        "sub/.replicant-tmp-gone".into(),
    ])
    .unwrap();
    let err = rx.clean(&["keep".into()]).unwrap_err().to_string();
    assert!(err.contains("not a temporary name"), "{err}");
    assert_eq!(names(&root), ["a", "keep", "sub"]);
    assert!(names(&root.join("sub")).is_empty());
}

#[test]
fn a_link_is_made_as_a_link_and_never_followed() {
    let t = tempfile::tempdir().unwrap();
    let outside = t.path().join("outside");
    write(&outside.join("victim"), "untouched");
    let root = t.path().join("unit");
    write(&root.join("was-file"), "x");
    std::os::unix::fs::symlink(&outside, root.join("out")).unwrap();
    let rx = Receiver::open(&root, false, None, None).unwrap();

    rx.symlink(&link("was-file", "elsewhere")).unwrap();
    assert_eq!(
        fs::read_link(root.join("was-file")).unwrap(),
        Path::new("elsewhere")
    );
    // Replacing the link `out` replaces the link, not what it points at.
    rx.symlink(&link("out", "nowhere")).unwrap();
    assert_eq!(
        fs::read_link(root.join("out")).unwrap(),
        Path::new("nowhere")
    );
    assert_eq!(
        fs::read_to_string(outside.join("victim")).unwrap(),
        "untouched"
    );
}

#[test]
fn nothing_is_written_through_a_link_or_outside_the_unit() {
    let t = tempfile::tempdir().unwrap();
    let outside = t.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let root = t.path().join("unit");
    fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("out")).unwrap();
    let rx = Receiver::open(&root, false, None, None).unwrap();
    for rel in ["out/x", "../escape", "/abs", "a/./b", ""] {
        let err = rx.file(&file(rel, 1, 1, 0o644), b"x");
        assert!(err.is_err(), "{rel:?} was written");
    }
    assert!(rx.mkdir(&dir("out/d", 0o755)).is_err());
    assert!(rx.delete(&[("out/x".into(), false)]).is_err() || !outside.join("x").exists());
    assert!(names(&outside).is_empty(), "{:?}", names(&outside));
    assert!(!t.path().join("escape").exists());
}

#[test]
fn home_refuses_what_its_policy_excludes() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().to_path_buf();
    write(&root.join("secrets/key"), "home's");
    let policy = Excludes::for_root(&["secrets".into(), "*.sock".into()]).unwrap();
    let rx = Receiver::open(&root, false, Some(&policy), None).unwrap();
    for rel in ["secrets/key", "a/b.sock"] {
        let err = rx
            .file(&file(rel, 1, 1, 0o644), b"x")
            .unwrap_err()
            .to_string();
        assert!(err.contains("excluded on home"), "{err}");
    }
    assert!(rx.delete(&[("secrets/key".into(), false)]).is_err());
    assert_eq!(
        fs::read_to_string(root.join("secrets/key")).unwrap(),
        "home's"
    );
}

#[test]
fn a_read_only_directory_is_filled_then_given_its_mode() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().to_path_buf();
    let rx = Receiver::open(&root, false, None, None).unwrap();
    let d = dir("ro", 0o555);
    rx.mkdir(&d).unwrap();
    rx.file(&file("ro/f", 1, 1, 0o644), b"x").unwrap();
    rx.dir_modes(std::slice::from_ref(&d)).unwrap();
    let m = Manifest::walk(&root, &none(), false).unwrap();
    assert_eq!(m.get("ro"), Some(&d));
    fs::set_permissions(root.join("ro"), fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn deletions_go_children_first_and_a_directory_holding_more_stays() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().to_path_buf();
    write(&root.join("a/b/c"), "x");
    write(&root.join("a/x"), "x");
    write(&root.join("k/.env"), "ignored: not in the list");
    write(&root.join("k/old"), "x");
    let rx = Receiver::open(&root, false, None, None).unwrap();
    let kept = rx
        .delete(&[
            ("k/old".into(), false),
            ("k".into(), true),
            ("gone".into(), false),
            ("a/x".into(), false),
            ("a/b/c".into(), false),
            ("a/b".into(), true),
            ("a".into(), true),
        ])
        .unwrap();
    assert_eq!(kept, ["k"]);
    assert_eq!(names(&root), ["k"]);
    assert_eq!(names(&root.join("k")), [".env"]);
}

#[test]
fn kind_changes_remove_the_old_entry_and_spare_what_is_not_synced() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().to_path_buf();
    write(&root.join("f2d"), "file becoming a dir");
    write(&root.join("d2f/inner"), "x");
    write(&root.join("held/inner"), "x");
    write(&root.join("held/target/junk"), "excluded: never listed");
    let rx = Receiver::open(&root, false, None, None).unwrap();

    rx.replace("f2d", EntryKind::File, &[]).unwrap();
    rx.mkdir(&dir("f2d", 0o755)).unwrap();
    rx.replace("d2f", EntryKind::Dir, &[("d2f/inner".into(), false)])
        .unwrap();
    rx.file(&file("d2f", 1, 1, 0o644), b"x").unwrap();
    assert!(root.join("f2d").is_dir() && root.join("d2f").is_file());

    let err = rx
        .replace("held", EntryKind::Dir, &[("held/inner".into(), false)])
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("still holding what this unit does not sync"),
        "{err}"
    );
    assert!(root.join("held/target/junk").exists());
}

#[test]
fn a_single_file_unit_is_written_at_its_root() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("cfg/settings.json");
    let rx = Receiver::open(&root, true, None, None).unwrap();
    let e = file("", 2, 42, 0o600);
    rx.file(&e, b"{}").unwrap();
    assert_eq!(Manifest::walk(&root, &none(), true).unwrap().entries, [e]);
    assert!(rx.file(&file("x", 1, 1, 0o644), b"x").is_err());
    assert_eq!(
        rx.delete(&[(String::new(), false)]).unwrap(),
        Vec::<String>::new()
    );
    assert!(!root.exists());
}

#[test]
fn a_symlinked_unit_root_is_written_where_it_points() {
    let t = tempfile::tempdir().unwrap();
    let real = t.path().join("vol/work2");
    fs::create_dir_all(&real).unwrap();
    let base = t.path().join("work2");
    std::os::unix::fs::symlink(&real, &base).unwrap();
    let rx = Receiver::open(&base, false, None, None).unwrap();
    rx.file(&file("a", 1, 1, 0o644), b"x").unwrap();
    assert!(real.join("a").is_file());
    assert!(
        fs::symlink_metadata(&base)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn a_name_too_long_for_its_temporary_still_arrives() {
    let t = tempfile::tempdir().unwrap();
    let name = "n".repeat(250);
    let rx = Receiver::open(t.path(), false, None, None).unwrap();
    rx.file(&file(&name, 1, 1, 0o644), b"x").unwrap();
    assert_eq!(names(t.path()), [name]);
}

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
