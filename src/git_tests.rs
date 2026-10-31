use super::*;
use crate::testutil;

#[test]
fn parses_ordinary_rename_unmerged_untracked_and_ignored() {
    let out = b"1 .M N... 100644 100644 100644 aaaa bbbb src/a.rs\0\
2 R. N... 100644 100644 100644 cccc dddd R100 new name.rs\0old.rs\0\
u UU N... 100644 100644 100644 100644 e1 e2 e3 conflict.txt\0\
? scratch/x y.txt\0\
! ignored.log\0";
    let e = parse_porcelain_v2(out).unwrap();
    assert_eq!(e.len(), 4);
    assert_eq!(e[0].path, "src/a.rs");
    assert!(e[0].worktree_changed && !e[0].worktree_deleted);
    assert_eq!(e[1].path, "new name.rs", "paths with spaces survive");
    assert_eq!(e[1].orig.as_deref(), Some("old.rs"));
    assert!(!e[1].worktree_changed, "staged-only rename");
    assert!(e[1].raw.contains("old.rs"));
    assert_eq!(e[2].path, "conflict.txt");
    assert!(e[2].worktree_changed);
    assert_eq!(e[3].path, "scratch/x y.txt");
}

#[test]
fn worktree_deletion_is_flagged() {
    let e = parse_porcelain_v2(b"1 .D N... 100644 100644 000000 aa aa gone\0").unwrap();
    assert!(e[0].worktree_deleted && e[0].worktree_changed);
}

#[test]
fn garbage_is_an_error() {
    assert!(parse_porcelain_v2(b"Z what\0").is_err());
    assert!(parse_porcelain_v2(b"1 .M\0").is_err());
    assert!(
        parse_porcelain_v2(b"2 R. N... 1 1 1 a b R100 x\0").is_err(),
        "missing orig"
    );
}

#[test]
fn status_of_a_real_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let r = testutil::repo(&tmp.path().join("r"));
    let clean = status(&r).unwrap();
    assert_eq!(clean.head, "ref: refs/heads/main");
    assert!(clean.refs.starts_with("refs/heads/main "), "{}", clean.refs);
    assert!(clean.entries.is_empty());

    testutil::write(&r.join("README"), "edited\n");
    testutil::write(&r.join("u.txt"), "u\n");
    let dirty = status(&r).unwrap();
    let paths: Vec<_> = dirty.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, vec!["README", "u.txt"]);
}

#[test]
fn status_does_not_write_the_index() {
    let tmp = tempfile::tempdir().unwrap();
    let r = testutil::repo(&tmp.path().join("r"));
    // Touch a tracked file without changing it: a refreshing `git status`
    // would rewrite .git/index to record the new stat data.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    testutil::write(&r.join("README"), "hello\n");
    let index = r.join(".git/index");
    let before = std::fs::read(&index).unwrap();
    let mtime = std::fs::metadata(&index).unwrap().modified().unwrap();
    status(&r).unwrap();
    assert_eq!(std::fs::read(&index).unwrap(), before);
    assert_eq!(
        std::fs::metadata(&index).unwrap().modified().unwrap(),
        mtime
    );
}

#[test]
fn stash_and_branches_appear_in_refs() {
    let tmp = tempfile::tempdir().unwrap();
    let r = testutil::repo(&tmp.path().join("r"));
    testutil::git(&r, &["branch", "side"]);
    testutil::write(&r.join("README"), "stash me\n");
    testutil::git(&r, &["stash", "-q"]);
    let s = status(&r).unwrap();
    assert!(s.refs.contains("refs/heads/side"));
    assert!(s.refs.contains("refs/stash"));
}

#[test]
fn locks_finds_git_locks_only() {
    let tmp = tempfile::tempdir().unwrap();
    let r = testutil::repo(&tmp.path().join("r"));
    testutil::write(&r.join("Cargo.lock"), "");
    testutil::write(&r.join(".git/index.lock"), "");
    testutil::write(&r.join(".git/refs/heads/main.lock"), "");
    testutil::write(&r.join(".git/worktrees/wt/index.lock"), "");
    assert_eq!(
        locks(&r).unwrap(),
        vec![".git/index.lock", ".git/refs/heads/main.lock"]
    );
}

#[test]
fn status_outside_a_repo_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(status(tmp.path()).is_err());
}

#[test]
fn a_non_utf8_path_is_refused_by_name_not_hashed_as_something_else() {
    let err = parse_porcelain_v2(b"1 .M N... 100644 100644 100644 aa bb ok.rs\0? caf\xe9.txt\0")
        .unwrap_err()
        .to_string();
    assert!(err.contains("not UTF-8"), "{err}");
    assert!(err.contains("caf"), "names the path: {err}");
    let err = parse_porcelain_v2(b"2 R. N... 100644 100644 100644 cc dd R100 new.rs\0\xffold\0")
        .unwrap_err()
        .to_string();
    assert!(err.contains("not UTF-8"), "a rename's source too: {err}");
}

#[test]
fn status_never_runs_a_repo_s_fsmonitor() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("r");
    testutil::repo(&repo);
    let ran = tmp.path().join("fsmonitor-ran");
    let hook = testutil::script(
        tmp.path(),
        "fsmonitor",
        &format!("touch {}\nexit 1", ran.display()),
    );
    testutil::git(
        &repo,
        &["config", "core.fsmonitor", &hook.display().to_string()],
    );
    status(&repo).unwrap();
    assert!(
        !ran.exists(),
        "git status started the repo's fsmonitor hook"
    );
}
