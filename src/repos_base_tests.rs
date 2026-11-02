//! Review P4: [`discover_base`] follows a symlinked base and nothing else.

use super::*;
use std::os::unix::fs::symlink;

fn none() -> Excludes {
    Excludes::default()
}

#[test]
fn a_plain_base_is_discovered_as_before() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f"), "x").unwrap();
    assert_eq!(
        discover_base(tmp.path(), &none()).unwrap(),
        discover(tmp.path(), &none()).unwrap()
    );
}

#[test]
fn a_dangling_base_does_not_exist() {
    let tmp = tempfile::tempdir().unwrap();
    let link = tmp.path().join("work2");
    symlink(tmp.path().join("gone"), &link).unwrap();
    let d = discover_base(&link, &none()).unwrap();
    assert!(!d.exists);
    assert_eq!(d, Discovery::default());
    // `discover` (a unit inside the tree) still sees the link itself.
    let inside = discover(&link, &none()).unwrap();
    assert!(inside.exists);
    assert_eq!(inside.loose[0].kind, Kind::Symlink);
}

#[test]
fn a_base_linked_to_a_file_is_that_file() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("settings.json"), "{}").unwrap();
    let link = tmp.path().join("link.json");
    symlink("settings.json", &link).unwrap();
    let d = discover_base(&link, &none()).unwrap();
    assert_eq!(d.loose.len(), 1);
    assert_eq!(d.loose[0].rel, "");
    assert_eq!(d.loose[0].kind, Kind::File);
    assert_eq!(d.loose[0].size, 2);
}

#[test]
fn a_link_loop_is_an_error_not_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    symlink(&b, &a).unwrap();
    symlink(&a, &b).unwrap();
    assert!(discover_base(&a, &none()).is_err());
}

#[test]
fn a_base_linked_to_a_repo_is_a_repo_and_names_walk_through_it() {
    let tmp = tempfile::tempdir().unwrap();
    let real = crate::testutil::repo(&tmp.path().join("vol/r"));
    std::fs::write(real.join("other"), "x").unwrap();
    let link = tmp.path().join("r");
    symlink(&real, &link).unwrap();
    assert_eq!(discover_base(&link, &none()).unwrap().repos, vec![""]);
    // The case walk, run on the unit root, sees the repo's names (it saw
    // nothing through a link before).
    let names = all_names(&link, &none()).unwrap();
    for n in ["README", "other", ".git/HEAD"] {
        assert!(names.contains(&n.to_string()), "{n}: {names:?}");
    }
}
