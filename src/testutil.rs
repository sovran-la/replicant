//! Fixture builders shared by the tests. Never compiled into the binary.
//!
//! Every fixture lives in a tempdir. git runs with the user's and system's
//! config switched off, so a hook or signing setting on the dev machine cannot
//! change what a test sees.

use std::path::{Path, PathBuf};
use std::process::Command;

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// A repo with one commit of `README`.
pub fn repo(path: &Path) -> PathBuf {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "-q"]);
    write(&path.join("README"), "hello\n");
    git(path, &["add", "README"]);
    git(path, &["commit", "-qm", "init"]);
    path.to_path_buf()
}

/// The fixture tree the task calls for, under `base`:
///
/// ```text
/// base/
///   alpha/            repo, clean
///   beta/             repo, dirty (modified + untracked), .git/index.lock held
///   nested/gamma/     repo
///   alpha-wt/         worktree of alpha (a .git *file* into alpha/.git/worktrees/)
///   notes.txt         loose
///   data/blob.bin     loose
///   target/junk       excluded by the work root's excludes
/// ```
pub fn tree(base: &Path) {
    let alpha = repo(&base.join("alpha"));
    let beta = repo(&base.join("beta"));
    repo(&base.join("nested/gamma"));
    git(
        &alpha,
        &["worktree", "add", "-q", "-b", "wt", "../alpha-wt"],
    );
    write(&beta.join("README"), "changed\n");
    write(&beta.join("new.txt"), "untracked\n");
    write(&beta.join(".git/index.lock"), "");
    write(&base.join("notes.txt"), "loose\n");
    write(&base.join("data/blob.bin"), "0123456789");
    write(&base.join("target/junk"), "build output");
}
