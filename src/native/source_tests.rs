use super::*;
use crate::native::manifest::tests::set_mtime;
use crate::testutil::write;
use std::io::Write as _;

#[test]
fn reads_what_is_there_now_and_skips_what_is_not() {
    let t = tempfile::tempdir().unwrap();
    let r = t.path();
    write(&r.join("a.txt"), "now");
    std::os::unix::fs::symlink("a.txt", r.join("l")).unwrap();
    std::fs::create_dir(r.join("d")).unwrap();
    let src = Source::open(r, false, None).unwrap();
    match src.read("a.txt").unwrap() {
        Read::Whole(e, b) => assert_eq!((e.size, b), (3, b"now".to_vec())),
        Read::Changed => panic!(),
    }
    match src.read("l").unwrap() {
        Read::Whole(e, b) => assert!(e.kind == EntryKind::Symlink && b.is_empty()),
        Read::Changed => panic!(),
    }
    assert!(matches!(src.read("gone").unwrap(), Read::Changed));
    assert!(
        matches!(src.read("d").unwrap(), Read::Changed),
        "a directory now"
    );
}

#[test]
fn never_reads_through_a_link_outside_or_against_policy() {
    let t = tempfile::tempdir().unwrap();
    write(&t.path().join("outside/secret"), "s");
    let r = t.path().join("unit");
    write(&r.join("keys/k"), "k");
    std::os::unix::fs::symlink(t.path().join("outside"), r.join("out")).unwrap();
    let policy = Excludes::for_root(&["keys".into()]).unwrap();
    let src = Source::open(&r, false, Some(&policy)).unwrap();
    for rel in [
        "out/secret",
        "../outside/secret",
        "/etc/passwd",
        "keys/k",
        "",
    ] {
        assert!(src.read(rel).is_err(), "{rel:?} was read");
    }
    // A link swapped in for a file is not followed by open().
    std::os::unix::fs::symlink(t.path().join("outside/secret"), r.join("swapped")).unwrap();
    assert!(open(&r.join("swapped")).is_none());
}

#[test]
fn unchanged_compares_size_and_mtime_of_the_open_file() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("f");
    write(&p, "abc");
    set_mtime(&p, 100, 1);
    let m = std::fs::symlink_metadata(&p).unwrap();
    let e = Entry::of(&p, "f", &m).unwrap();
    let f = open(&p).unwrap();
    assert!(unchanged(&f, &e));
    set_mtime(&p, 100, 2);
    assert!(!unchanged(&f, &e), "a touch is a change");
    set_mtime(&p, 100, 1);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&p)
        .unwrap()
        .write_all(b"d")
        .unwrap();
    set_mtime(&p, 100, 1);
    assert!(!unchanged(&f, &e), "so is a size");
}

#[test]
fn a_single_file_unit_reads_only_its_root() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("settings.json");
    write(&p, "{}");
    let src = Source::open(&p, true, None).unwrap();
    assert!(matches!(src.read("").unwrap(), Read::Whole(_, b) if b == b"{}"));
    assert!(src.read("x").is_err());
}
