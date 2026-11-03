//! The list operations rollback is made of: exactly the list, never
//! through a link, metadata kept.

use super::*;
use crate::native::manifest::Manifest;
use crate::native::manifest::tests::set_mtime;
use crate::testutil::write;
use std::os::unix::fs::PermissionsExt;

fn fixture(root: &Path) {
    write(&root.join("keep"), "k\n");
    write(&root.join("a*b"), "wild name, listed\n");
    write(&root.join("ab"), "matches a*b as a glob, not listed\n");
    write(&root.join("old/added"), "listed\n");
    write(&root.join("old/keep"), "not listed\n");
    std::os::unix::fs::symlink("keep", root.join("old/addlink")).unwrap();
    write(&root.join("new/f"), "listed\n");
    write(&root.join("new/sub/g"), "listed\n");
    write(&root.join("half/listed"), "listed\n");
    write(&root.join("half/other"), "not listed\n");
}

fn rels(root: &Path) -> Vec<String> {
    let none = crate::exclude::Excludes::default();
    Manifest::walk(root, &none, false)
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.rel)
        .collect()
}

#[test]
fn delete_listed_removes_exactly_the_list() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("unit");
    fixture(&root);
    write(&tmp.path().join("elsewhere/x"), "behind a link\n");
    std::os::unix::fs::symlink(tmp.path().join("elsewhere"), root.join("linked")).unwrap();
    let list: Vec<(String, bool)> = [
        ("a*b", false),
        ("old/added", false),
        ("old/addlink", false),
        ("new", true),
        ("new/f", false),
        ("new/sub", true),
        ("new/sub/g", false),
        ("half", true),
        ("half/listed", false),
        ("missing/dir/x", false),
        ("linked/x", false),
        ("../escape", false),
    ]
    .iter()
    .map(|(p, d)| (p.to_string(), *d))
    .collect();
    let done = delete_listed(&root, &list).unwrap();
    assert_eq!(
        rels(&root),
        [
            "ab",
            "half",
            "half/other",
            "keep",
            "linked",
            "old",
            "old/keep"
        ],
        "only the list went; a listed directory with unlisted contents stays; \
         nothing missing was created"
    );
    let gone: Vec<&str> = done.changes.iter().map(|c| c.path.as_str()).collect();
    for p in [
        "a*b",
        "old/added",
        "old/addlink",
        "new/f",
        "new/sub/g",
        "new/sub",
        "new",
        "half/listed",
    ] {
        assert!(gone.contains(&p), "{p} in {gone:?}");
    }
    assert!(!gone.contains(&"half") && !gone.contains(&"linked/x"));
    assert!(
        tmp.path().join("elsewhere/x").exists(),
        "never through a link"
    );
}

#[test]
fn copy_listed_copies_files_and_links_with_their_metadata_and_nothing_else() {
    let tmp = tempfile::tempdir().unwrap();
    let from = tmp.path().join("unit");
    fixture(&from);
    std::fs::set_permissions(
        from.join("old/added"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    set_mtime(&from.join("old/added"), 1_234_567, 891);
    std::fs::set_permissions(from.join("new/sub"), std::fs::Permissions::from_mode(0o750)).unwrap();
    let to = tmp.path().join("backup/run-1");
    let files: Vec<String> = ["a*b", "old/added", "old/addlink", "new/sub/g"]
        .map(String::from)
        .into();
    let done = copy_listed(&from, &to, &files).unwrap();
    assert_eq!(done.changes.len(), 4);
    assert_eq!(
        rels(&to),
        [
            "a*b",
            "new",
            "new/sub",
            "new/sub/g",
            "old",
            "old/added",
            "old/addlink"
        ]
    );
    let none = crate::exclude::Excludes::default();
    let (a, b) = (
        Manifest::walk(&from, &none, false).unwrap(),
        Manifest::walk(&to, &none, false).unwrap(),
    );
    for p in ["old/added", "old/addlink", "new/sub", "new/sub/g"] {
        assert_eq!(
            a.get(p),
            b.get(p),
            "{p}: mode, mtime to the ns, link target"
        );
    }
    // A directory already there keeps its own mode.
    std::fs::set_permissions(to.join("new/sub"), std::fs::Permissions::from_mode(0o700)).unwrap();
    copy_listed(&from, &to, &["new/sub/g".into()]).unwrap();
    let mode = std::fs::metadata(to.join("new/sub"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o700);
    // Restoring over what is there replaces it whole.
    write(&from.join("keep"), "changed\n");
    copy_listed(&from, &to, &["keep".into()]).unwrap();
    assert_eq!(
        std::fs::read_to_string(to.join("keep")).unwrap(),
        "changed\n"
    );
}

#[test]
fn copy_listed_refuses_what_it_cannot_copy_faithfully() {
    let tmp = tempfile::tempdir().unwrap();
    let from = tmp.path().join("unit");
    write(&from.join("d/f"), "x");
    write(&tmp.path().join("elsewhere/f"), "x");
    std::os::unix::fs::symlink(tmp.path().join("elsewhere"), from.join("l")).unwrap();
    let to = tmp.path().join("to");
    for bad in ["d", "l/f", "../unit/d/f", "missing"] {
        assert!(copy_listed(&from, &to, &[bad.into()]).is_err(), "{bad}");
    }
}

#[test]
fn no_directories_to_bring_back_touches_nothing() {
    let t = tempfile::tempdir().unwrap();
    let missing = t.path().join("missing");
    open_dirs(&missing, &[]).unwrap();
    set_dir_modes(&missing, &[]).unwrap();
    assert!(!missing.exists());
}
