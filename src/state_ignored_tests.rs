//! The state hash and git-ignored files (PLAN.md rev 7): what the repo's own
//! `.gitignore` files ignore is not state; what anything else ignores still
//! syncs, so it must be.

use super::*;
use crate::testutil::{self, git, write};

fn h(abs: &Path) -> RepoFacts {
    repo(abs, &Excludes::default()).unwrap()
}

fn bump_mtime(path: &Path) {
    let f = std::fs::File::options().write(true).open(path).unwrap();
    let was = f.metadata().unwrap().modified().unwrap();
    f.set_modified(was + std::time::Duration::from_secs(1))
        .unwrap();
}

#[test]
fn files_ignored_by_the_repos_gitignore_are_not_state() {
    let tmp = tempfile::tempdir().unwrap();
    let r = testutil::repo(&tmp.path().join("r"));
    write(&r.join(".gitignore"), "*.log\n.env*\ntarget/\n");
    write(&r.join("sub/.gitignore"), "secret\n");
    git(&r, &["add", "."]);
    git(&r, &["commit", "-qm", "ignores"]);
    let before = h(&r);

    for f in ["x.log", ".env.local", "target/debug/app", "sub/secret"] {
        write(&r.join(f), "one");
    }
    let made = h(&r);
    assert_eq!(made.state, before.state, "creating ignored files");
    for f in ["x.log", ".env.local", "target/debug/app", "sub/secret"] {
        write(&r.join(f), "a longer rewrite");
        bump_mtime(&r.join(f));
    }
    let after = h(&r);
    assert_eq!(after.state, before.state, "rewriting ignored files");
    assert!(after.dirty.is_empty(), "{:?}", after.dirty);
    assert_eq!(after.files, before.files, "ignored files are not counted");

    // The same kind of file, not ignored, is state: the test can tell.
    write(&r.join("x.txt"), "one");
    assert_ne!(h(&r).state, before.state);
}

#[test]
fn a_file_only_the_global_gitignore_hides_is_state() {
    let tmp = tempfile::tempdir().unwrap();
    let r = testutil::repo(&tmp.path().join("r"));
    let global = tmp.path().join("global-ignore");
    write(&global, "glob.txt\n");
    // The repo's config stands in for ~/.gitconfig: replicant's `-c` must
    // win over both.
    git(
        &r,
        &["config", "core.excludesFile", global.to_str().unwrap()],
    );
    let before = h(&r);
    write(&r.join("glob.txt"), "wip");
    let after = h(&r);
    assert_ne!(after.state, before.state);
    assert_eq!(after.dirty, vec!["glob.txt".to_string()]);
}

/// The one exclusion source `git status` cannot be told to skip. Pinned so
/// it is a decision, not a surprise: such a file syncs but is not hashed.
#[test]
fn a_file_only_info_exclude_hides_is_invisible_by_design() {
    let tmp = tempfile::tempdir().unwrap();
    let r = testutil::repo(&tmp.path().join("r"));
    write(&r.join(".git/info/exclude"), "info.txt\n");
    let before = h(&r);
    write(&r.join("info.txt"), "wip");
    assert_eq!(h(&r).state, before.state);
    assert!(
        crate::repos::ignored(&r).unwrap().is_empty(),
        "but it syncs"
    );
}
