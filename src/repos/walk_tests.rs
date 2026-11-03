//! The parallel walk: deterministic, and the same answer the serial walk gave.

use super::super::*;
use crate::testutil;
use std::os::unix::fs::PermissionsExt;

/// The serial walk `discover` used before it went parallel, kept as the
/// reference the parallel one must equal.
fn serial(abs: &Path, rel: &str, ex: &Excludes, d: &mut Discovery) {
    match git_marker(abs) {
        GitMarker::Repo => return d.repos.push(rel.to_string()),
        GitMarker::Worktree => return d.worktrees.push(rel.to_string()),
        GitMarker::None | GitMarker::OtherFile => {}
    }
    for item in fs::read_dir(abs).unwrap() {
        let item = item.unwrap();
        let name = item.file_name().into_string().unwrap();
        let child_rel = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        let meta = fs::symlink_metadata(item.path()).unwrap();
        if ex.matches(&child_rel, meta.is_dir()) {
            continue;
        }
        if meta.is_dir() {
            let ordinary = matches!(
                git_marker(&item.path()),
                GitMarker::None | GitMarker::OtherFile
            );
            serial(&item.path(), &child_rel, ex, d);
            if ordinary {
                d.loose
                    .push(entry(&item.path(), &child_rel, &meta).unwrap());
            }
        } else {
            d.loose
                .push(entry(&item.path(), &child_rel, &meta).unwrap());
        }
    }
}

fn reference(base: &Path, ex: &Excludes) -> Discovery {
    let mut d = Discovery {
        exists: true,
        ..Default::default()
    };
    serial(base, "", ex, &mut d);
    d.repos.sort();
    d.worktrees.sort();
    d.loose.sort();
    d
}

/// Wide enough that every walker thread gets work.
fn wide_tree(root: &Path) {
    testutil::tree(root);
    for i in 0..40 {
        for j in 0..20 {
            testutil::write(&root.join(format!("wide/d{i}/f{j}.txt")), "x");
        }
        testutil::write(&root.join(format!("wide/d{i}/target/junk")), "x");
        testutil::write(&root.join(format!("wide/d{i}/.hidden")), "x");
        if i % 7 == 0 {
            testutil::write(
                &root.join(format!("wide/d{i}/repo{i}/.git/HEAD")),
                "ref: x\n",
            );
            testutil::write(&root.join(format!("wide/d{i}/repo{i}/src.rs")), "x");
        }
    }
    std::os::unix::fs::symlink("../notes.txt", root.join("wide/link")).unwrap();
}

#[test]
fn two_discoveries_of_the_same_tree_are_identical() {
    let tmp = tempfile::tempdir().unwrap();
    wide_tree(tmp.path());
    let ex = Excludes::for_root(&["target".into()]).unwrap();
    let first = discover(tmp.path(), &ex).unwrap();
    for _ in 0..5 {
        assert_eq!(discover(tmp.path(), &ex).unwrap(), first);
    }
    assert_eq!(
        first.repos.len(),
        3 + 6,
        "fixture repos plus wide/d0,7,…,35"
    );
    assert!(first.loose.len() > 40 * 22);
}

#[test]
fn parallel_walk_equals_the_serial_walk() {
    let tmp = tempfile::tempdir().unwrap();
    wide_tree(tmp.path());
    for ex in [
        Excludes::default(),
        Excludes::for_root(&["target".into(), "*.txt".into()]).unwrap(),
        Excludes::for_root(&["**/d1*/".into(), ".hidden".into()]).unwrap(),
    ] {
        assert_eq!(
            discover(tmp.path(), &ex).unwrap(),
            reference(tmp.path(), &ex)
        );
    }
}

/// Replicant has to see everything a transfer would move, so none of `ignore`'s
/// filters may be on.
#[test]
fn hidden_and_ignored_files_are_seen() {
    let tmp = tempfile::tempdir().unwrap();
    let b = tmp.path();
    testutil::write(&b.join(".gitignore"), "*\n");
    testutil::write(&b.join(".ignore"), "*\n");
    testutil::write(&b.join("sub/.rgignore"), "*\n");
    testutil::write(&b.join(".env"), "SECRET=1");
    testutil::write(&b.join("sub/ignored.log"), "x");
    testutil::write(&b.join(".cache/x"), "x");
    let d = discover(b, &Excludes::default()).unwrap();
    let rels: Vec<&str> = d.loose.iter().map(|e| e.rel.as_str()).collect();
    assert_eq!(
        rels,
        [
            ".cache",
            ".cache/x",
            ".env",
            ".gitignore",
            ".ignore",
            "sub",
            "sub/.rgignore",
            "sub/ignored.log"
        ]
    );
}

#[test]
fn deep_worktrees_and_repos_are_not_descended() {
    let tmp = tempfile::tempdir().unwrap();
    let b = tmp.path();
    testutil::write(
        &b.join("a/b/c/wt/.git"),
        "gitdir: /elsewhere/.git/worktrees/wt\n",
    );
    testutil::write(&b.join("a/b/c/wt/inside"), "x");
    testutil::write(&b.join("a/r/.git/HEAD"), "ref: x\n");
    testutil::write(&b.join("a/r/inner/.git/HEAD"), "x");
    let d = discover(b, &Excludes::default()).unwrap();
    assert_eq!(d.worktrees, ["a/b/c/wt"]);
    assert_eq!(
        d.repos,
        ["a/r"],
        "a repo inside a repo is the outer's content"
    );
    assert!(d.loose.iter().all(|e| !e.rel.starts_with("a/b/c/wt")));
    assert!(d.loose.iter().all(|e| !e.rel.starts_with("a/r")));
}

#[test]
fn a_repo_under_an_excluded_directory_is_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    testutil::write(&tmp.path().join("vendor/dep/.git/HEAD"), "ref: x\n");
    let d = discover(tmp.path(), &Excludes::new(&["vendor/"]).unwrap()).unwrap();
    assert!(d.repos.is_empty() && d.loose.is_empty(), "{d:?}");
}

/// As before: an unreadable directory fails discovery rather than reading as
/// empty, which would look like every file in it was deleted.
#[test]
fn an_unreadable_directory_is_an_error() {
    if unsafe { libc::geteuid() } == 0 {
        return; // root reads through mode 000
    }
    let tmp = tempfile::tempdir().unwrap();
    let locked = tmp.path().join("locked");
    testutil::write(&locked.join("x"), "x");
    testutil::write(&tmp.path().join("open/y"), "y");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let r = discover(tmp.path(), &Excludes::default());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    let err = r.expect_err("an unreadable directory must not read as empty");
    assert!(matches!(err, crate::Error::Io(_)), "{err}");
}

#[test]
fn a_git_directory_without_head_is_not_yet_a_repo() {
    // What an interrupted pull of a new repo leaves before its HEAD: plain
    // content, not a repo whose status cannot be read (which would fail
    // every plan, the resume that completes it included).
    let tmp = tempfile::tempdir().unwrap();
    testutil::write(&tmp.path().join("half/.git/objects/ab/cdef"), "x");
    testutil::write(
        &tmp.path().join("whole/.git/HEAD"),
        "ref: refs/heads/main\n",
    );
    assert_eq!(
        crate::repos::git_marker(&tmp.path().join("half")),
        crate::repos::GitMarker::None
    );
    let d = discover(tmp.path(), &Excludes::for_root(&[]).unwrap()).unwrap();
    assert_eq!(d.repos, ["whole"]);
    assert!(
        d.loose.iter().any(|e| e.rel == "half/.git/objects/ab/cdef"),
        "{d:?}"
    );
}
