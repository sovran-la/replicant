//! The list operations home's backups and rollbacks are made of, against
//! the system rsync.

use super::*;

/// A tree for the list operations.
fn listed_fixture(root: &Path) {
    testutil::write(&root.join("keep"), "k\n");
    testutil::write(&root.join("a*b"), "wild name, listed\n");
    testutil::write(&root.join("ab"), "matches a*b as a glob, not listed\n");
    testutil::write(&root.join("old/added"), "listed\n");
    testutil::write(&root.join("old/keep"), "not listed\n");
    std::os::unix::fs::symlink("keep", root.join("old/addlink")).unwrap();
    testutil::write(&root.join("new/f"), "listed\n");
    testutil::write(&root.join("new/sub/g"), "listed\n");
    testutil::write(&root.join("half/listed"), "listed\n");
    testutil::write(&root.join("half/other"), "not listed\n");
}

#[test]
fn delete_listed_removes_exactly_the_list() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("unit");
    listed_fixture(&root);
    let paths: Vec<(String, bool)> = [
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
    ]
    .iter()
    .map(|(p, d)| (p.to_string(), *d))
    .collect();
    // An ancestor that is a symlink to a directory is never replaced.
    testutil::write(&tmp.path().join("elsewhere/x"), "behind a link\n");
    std::os::unix::fs::symlink(tmp.path().join("elsewhere"), root.join("linked")).unwrap();
    let scratch = tmp.path().join("scratch");
    let ran = delete_listed(&runner(&scratch), None, &root, &paths).unwrap();
    assert!(ran.ok(), "{ran:?}");
    let left: Vec<String> = tree(&root).into_keys().collect();
    assert_eq!(
        left,
        [
            "ab",
            "half/",
            "half/other",
            "keep",
            "linked",
            "old/",
            "old/keep"
        ],
        "only the listed paths went; a listed directory with unlisted \
         contents stays; a missing ancestor is not created"
    );
    let deleted: Vec<&str> = ran
        .itemized
        .changes
        .iter()
        .filter(|c| c.kind == ChangeKind::Delete)
        .map(|c| c.path.as_str())
        .collect();
    for p in [
        "a*b",
        "old/added",
        "old/addlink",
        "new/f",
        "new/sub/g",
        "half/listed",
    ] {
        assert!(deleted.contains(&p), "{p} in {deleted:?}");
    }
    assert_eq!(
        std::fs::read_dir(&scratch).unwrap().count(),
        0,
        "skeleton and list removed"
    );
    assert!(tmp.path().join("elsewhere/x").exists());
    let err = delete_listed(&runner(&scratch), Some("h"), &root, &paths)
        .unwrap_err()
        .to_string();
    assert!(err.contains("local"), "{err}");
}

#[test]
fn a_single_file_unit_is_deleted_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    testutil::write(&home.join("settings.json"), "{}\n");
    testutil::write(&home.join("settings.json.bak"), "sibling\n");
    testutil::write(&home.join("other/x"), "sibling\n");
    let x = Transfer {
        is_file: true,
        source_absent: true,
        ..transfer(
            Direction::Push,
            &tmp.path().join("leaf/settings.json"),
            &home.join("settings.json"),
        )
    };
    let r = runner(&tmp.path().join("scratch"));
    let planned = x.dry_run(&r).unwrap();
    let ran = x.apply(&r).unwrap();
    assert!(ran.ok(), "{ran:?}");
    assert_eq!(
        ran.itemized.changes, planned.changes,
        "the unit itself, \"\""
    );
    let left: Vec<String> = tree(&home).into_keys().collect();
    assert_eq!(left, ["other/", "other/x", "settings.json.bak"]);
}

#[test]
fn copy_listed_copies_files_with_attributes_and_nothing_else() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let from = tmp.path().join("unit");
    listed_fixture(&from);
    std::fs::set_permissions(
        from.join("old/added"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let to: PathBuf = tmp.path().join("backup/run-1");
    std::fs::create_dir_all(&to).unwrap();
    let files: Vec<String> = ["a*b", "old/added", "old/addlink", "new/sub/g"]
        .map(String::from)
        .into();
    let ran = copy_listed(&runner(&tmp.path().join("scratch")), &from, &to, &files).unwrap();
    assert!(ran.ok(), "{ran:?}");
    let got = tree(&to);
    let files_got: Vec<&String> = got.keys().filter(|k| !k.ends_with('/')).collect();
    assert_eq!(files_got, ["a*b", "new/sub/g", "old/added", "old/addlink"]);
    assert_eq!(got["old/addlink"], "-> keep");
    let m = |p: &Path| std::fs::metadata(p).unwrap();
    assert_eq!(m(&to.join("old/added")).permissions().mode() & 0o777, 0o600);
    // openrsync keeps whole seconds only (measured).
    let secs = |p: &Path| {
        m(p).modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    };
    assert_eq!(secs(&to.join("old/added")), secs(&from.join("old/added")));
}

#[test]
fn a_name_with_a_line_break_is_refused_not_split() {
    let tmp = tempfile::tempdir().unwrap();
    let r = runner(&tmp.path().join("scratch"));
    let err = copy_listed(&r, tmp.path(), tmp.path(), &["a\nb".into()])
        .unwrap_err()
        .to_string();
    assert!(err.contains("line break"), "{err}");
}

#[test]
fn literal_rules_escape_wildcards() {
    assert_eq!(Excludes::literal_rule("a/b", false), "/a/b");
    assert_eq!(Excludes::literal_rule("a", true), "/a/");
    assert_eq!(Excludes::literal_rule("a*b", false), "/a\\*b");
}
