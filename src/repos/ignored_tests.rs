//! [`ignored`] against git itself: for a fixture repo with nested
//! `.gitignore` files, negations, anchored and dir-only rules (and the traps
//! around them), the files inside replicant's list must be exactly the files
//! `git ls-files --others --ignored --exclude-per-directory=.gitignore` names:
//! git reading only the repo's own `.gitignore` files, as replicant does.

use super::*;
use crate::testutil::{self, git, write};
use std::collections::BTreeSet;
use std::path::PathBuf;

const ROOT_IGNORE: &str = "\
# anchored: only the top-level build
/build
# dir-only: not the file sub/cache
cache/
*.log
!keep.log
.env*
# an ignored directory cannot be re-included into
logs/
!logs/keep
docs/**/gen/
";

const SUB_IGNORE: &str = "\
secret.txt
/local
!z.log
";

/// Files of the fixture (relative to the repo), each written with one byte.
const FILES: &[&str] = &[
    "a.txt",
    "x.log",
    "keep.log",
    "UP.LOG",
    ".env.local",
    "build/o",
    "cache/c",
    "logs/keep",
    "logs/old",
    "docs/a/b/gen/out.html",
    "docs/gen/top.html",
    "docs/readme.md",
    "sub/build/o",
    "sub/cache",
    "sub/secret.txt",
    "sub/deep/secret.txt",
    "sub/local",
    "sub/deep/local",
    "sub/y.log",
    "sub/z.log",
    "sub/z.keep",
    "newdir/n.log",
    "newdir/ok",
    "glob.txt",
    "info.txt",
    "linked/x.tmp",
    "nested/n.log",
    "nested/n.tmp",
];

fn fixture(root: &Path) -> PathBuf {
    let r = testutil::repo(&root.join("r"));
    write(&r.join(".gitignore"), ROOT_IGNORE);
    write(&r.join("sub/.gitignore"), SUB_IGNORE);
    for f in FILES {
        write(&r.join(f), "x");
    }
    // A nested repo: git applies neither the parent's rules inside it nor
    // its own .gitignore to the parent's listing.
    git(&r.join("nested"), &["init", "-q"]);
    write(&r.join("nested/.gitignore"), "*.tmp\n");
    // A symlinked .gitignore is not read by git (>= 2.32), nor here.
    write(&r.join("elsewhere"), "*.tmp\n");
    std::os::unix::fs::symlink("../elsewhere", r.join("linked/.gitignore")).unwrap();
    // Exclusion sources that must not count.
    write(&r.join(".git/info/exclude"), "info.txt\n");
    let global = root.join("global-ignore");
    write(&global, "glob.txt\n");
    git(
        &r,
        &["config", "core.excludesFile", global.to_str().unwrap()],
    );
    git(&r, &["add", ".gitignore", "sub/.gitignore", "README"]);
    git(&r, &["commit", "-qm", "ignores"]);
    r
}

/// What git ignores, file by file, reading only in-repo `.gitignore` files.
fn gits(r: &Path) -> BTreeSet<String> {
    let out = git(
        r,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-per-directory=.gitignore",
        ],
    );
    out.split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Every file of the fixture inside one of `list`'s entries.
fn covered(list: &[String]) -> BTreeSet<String> {
    let mut all: Vec<&str> = FILES.to_vec();
    all.extend(["elsewhere", "linked/.gitignore", "nested/.gitignore"]);
    all.into_iter()
        .filter(|f| {
            list.iter().any(|e| match e.strip_suffix('/') {
                Some(dir) => crate::paths::is_within(f, dir),
                None => e == f,
            })
        })
        .map(str::to_string)
        .collect()
}

fn agree(r: &Path) -> Vec<String> {
    let ours = ignored(r).unwrap();
    assert_eq!(covered(&ours), gits(r), "replicant (left) vs git (right)");
    ours
}

#[test]
fn agrees_with_git_on_nested_negated_anchored_and_dir_only_rules() {
    let tmp = tempfile::tempdir().unwrap();
    let r = fixture(tmp.path());
    git(&r, &["config", "core.ignorecase", "false"]);
    let ours = agree(&r);
    let want: Vec<&str> = vec![
        ".env.local",
        "build/",
        "cache/",
        "docs/a/b/gen/",
        "docs/gen/",
        "logs/",
        "newdir/n.log",
        "sub/deep/secret.txt",
        "sub/local",
        "sub/secret.txt",
        "sub/y.log",
        "x.log",
    ];
    assert_eq!(ours, want);
    // Spelled out, the traps each rule sets:
    for kept in [
        "keep.log",     // negation
        "sub/z.log",    // negation in a nested .gitignore
        "sub/build/o",  // `/build` is anchored to the top
        "sub/cache",    // `cache/` is directories only
        "UP.LOG",       // case-sensitive here
        "info.txt",     // .git/info/exclude does not count
        "glob.txt",     // nor the global gitignore
        "linked/x.tmp", // nor a symlinked .gitignore
        "nested/n.tmp", // nor a nested repo's own rules
        "nested/n.log", // and the parent's rules stop at it
        "a.txt",
        ".gitignore",
    ] {
        assert!(!covered(&ours).contains(kept), "{kept} must sync");
    }
}

#[test]
fn a_directory_appears_only_when_a_rule_ignores_it() {
    // sub/deep holds nothing but ignored files; git --directory would name
    // `sub/deep/`, which unioned with the other side would hide its WIP.
    let tmp = tempfile::tempdir().unwrap();
    let r = fixture(tmp.path());
    let ours = ignored(&r).unwrap();
    assert!(!ours.iter().any(|e| e == "sub/deep/"), "{ours:?}");
    assert!(ours.iter().any(|e| e == "sub/deep/secret.txt"));
}

#[test]
fn core_ignorecase_is_honored_like_git() {
    let tmp = tempfile::tempdir().unwrap();
    let r = fixture(tmp.path());
    git(&r, &["config", "core.ignorecase", "true"]);
    assert!(ignore_case(&r));
    let ours = agree(&r);
    assert!(ours.iter().any(|e| e == "UP.LOG"), "{ours:?}");
    git(&r, &["config", "core.ignorecase", "false"]);
    assert!(!ignore_case(&r));
    assert!(!agree(&r).iter().any(|e| e == "UP.LOG"));
}

#[test]
fn ignore_case_parsing() {
    let tmp = tempfile::tempdir().unwrap();
    let r = tmp.path();
    let cases: &[(&str, bool)] = &[
        ("", false),
        ("[core]\n\tignorecase = true\n", true),
        ("[Core]\n  IgnoreCase=Yes ; comment\n", true),
        ("[core]\n\tignorecase\n", true),
        ("[core]\n\tignorecase = true\n\tignorecase = false\n", false),
        (
            "[core]\n\tbare = false\n[other]\n\tignorecase = true\n",
            false,
        ),
        ("[core \"x\"]\n\tignorecase = true\n", false),
        ("[core]\n\tignorecase = 0\n", false),
        ("[core]\n\tignorecase = on # c\n", true),
    ];
    for (text, want) in cases {
        write(&r.join(".git/config"), text);
        assert_eq!(ignore_case(r), *want, "{text:?}");
    }
    std::fs::remove_file(r.join(".git/config")).unwrap();
    assert!(!ignore_case(r), "no config");
}

#[test]
fn a_repo_without_gitignore_ignores_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let r = testutil::repo(&tmp.path().join("r"));
    write(&r.join("a/b.log"), "x");
    assert!(ignored(&r).unwrap().is_empty());
}

#[test]
fn a_missing_repo_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(ignored(&tmp.path().join("nope")).is_err());
}

/// What rsync would send from `r` with `ex`, without `.git` and its insides.
fn rsync_sends(r: &Path, ex: &crate::exclude::Excludes) -> BTreeSet<String> {
    let dst = r.parent().unwrap().join("dst");
    let out = std::process::Command::new("rsync")
        .args(["-n", "-a", "-8", "--out-format=%n"])
        .args(ex.rsync_args())
        .arg(format!("{}/", r.display()))
        .arg(format!("{}/", dst.display()))
        .output()
        .expect("the differential test needs rsync on PATH");
    assert!(out.status.success(), "rsync failed: {out:?}");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| l.trim_end_matches('/').to_string())
        .filter(|l| !l.is_empty() && l != ".")
        .filter(|l| !l.split('/').any(|c| c == ".git"))
        .collect()
}

/// The list rsync is handed and the list the case walk honors agree on the
/// whole fixture: neither sees an ignored path, both see everything else,
/// negated, anchored-elsewhere and dir-only-but-a-file paths included.
#[test]
fn rsync_and_the_case_walk_skip_exactly_the_ignored_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let r = fixture(tmp.path());
    let list = ignored(&r).unwrap();
    let ex = crate::units::with_ignored(crate::exclude::Excludes::for_root(&[]).unwrap(), &list);
    let walked: BTreeSet<String> = super::super::all_names(&r, &ex)
        .unwrap()
        .into_iter()
        .filter(|l| !l.split('/').any(|c| c == ".git"))
        .collect();
    let sent = rsync_sends(&r, &ex);
    assert_eq!(walked, sent, "case walk (left) vs rsync (right)");
    for f in covered(&list) {
        assert!(!sent.contains(&f), "{f} is ignored but sent");
    }
    for f in [
        "keep.log",
        "sub/z.log",
        "sub/build/o",
        "sub/cache",
        "sub/deep/local",
    ] {
        assert!(sent.contains(f), "{f} must sync: {sent:?}");
    }
    assert!(sent.contains(".gitignore") && sent.contains("sub/.gitignore"));
}
