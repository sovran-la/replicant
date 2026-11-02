//! Differential test: replicant's matcher against the system rsync.
//!
//! For each pattern list, the paths [`Excludes::path_excluded`] drops from a
//! fixture tree must be exactly the paths `rsync -n -a --itemize-changes`
//! leaves out when given [`Excludes::rsync_args`]. On macOS the system rsync
//! is openrsync (protocol 29); on Linux it is rsync 3. The translation targets
//! what both read the same way, so the test runs unchanged against either and
//! names the one it ran against when it fails.

use super::*;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// The fixture: `x/` is a directory, `@x` a symlink to `.`, anything else a
/// file. Names repeat at several depths on purpose.
const TREE: &[&str] = &[
    "target/",
    "target/debug.o",
    "app/",
    "app/target/",
    "app/target/x",
    "app/deep/",
    "app/deep/er/",
    "app/deep/er/target",
    "app/notes.log",
    "app/build",
    "build/",
    "build/out",
    "@linkdir",
    "top",
    "app/top",
    "a/",
    "a/b",
    "a/x/",
    "a/x/b/",
    "a/x/b/f",
    "a/x/y/",
    "a/x/y/b",
    "x/",
    "x/a/",
    "x/a/b",
    "x/a.b",
    "x/a0b",
    "agents/",
    "agents/state/",
    "agents/state/db",
    ".sovran/",
    ".sovran/agents/",
    ".sovran/agents/state/",
    ".sovran/agents/state/db",
    "src/",
    "src/main.rs",
    "src/inner/",
    "src/inner/mod.rs",
    "lib/",
    "lib/src/",
    "lib/src/x.rs",
    "gen/",
    "gen/a.c",
    "p/",
    "p/gen/",
    "p/gen/b.c",
    "p/gen/b.h",
    "cache/",
    "cache/one",
    "cache/sub/",
    "cache/sub/two",
    "v1",
    "v2/",
    "vx",
    ".git/",
    ".git/HEAD",
    ".git/index.lock",
    ".git/refs/",
    ".git/refs/heads/",
    ".git/refs/heads/main.lock",
    ".git/worktrees/",
    ".git/worktrees/wt/",
    ".git/worktrees/wt/HEAD",
    "nested/",
    "nested/.git/",
    "nested/.git/HEAD.lock",
    "nested/.git/config",
    "nested/Cargo.lock",
    "we\\ird",
    "st*r",
    "*lit",
    "sp ace",
    "{brace}",
    "q/",
    "q/x/",
    "q/x/m/",
    "q/x/m/n/",
    "q/x/m/n/y",
    "q/x/y",
    "q/y",
];

fn build(root: &Path) {
    for p in TREE {
        if let Some(link) = p.strip_prefix('@') {
            std::os::unix::fs::symlink(".", root.join(link)).unwrap();
        } else if let Some(dir) = p.strip_suffix('/') {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        } else {
            std::fs::write(root.join(p), "x").unwrap();
        }
    }
}

fn all() -> BTreeSet<(String, bool)> {
    TREE.iter()
        .map(|p| {
            let p = p.trim_start_matches('@');
            (p.trim_end_matches('/').to_string(), p.ends_with('/'))
        })
        .collect()
}

fn ours(ex: &Excludes) -> BTreeSet<String> {
    all()
        .into_iter()
        .filter(|(p, d)| ex.path_excluded(p, *d))
        .map(|(p, _)| p)
        .collect()
}

fn flavour() -> String {
    let out = Command::new("rsync").arg("--version").output();
    let out = out.expect("the differential test needs rsync on PATH");
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    text.lines().next().unwrap_or("unknown rsync").to_string()
}

fn theirs(src: &Path, ex: &Excludes) -> BTreeSet<String> {
    let dst = src.parent().unwrap().join("dst");
    let out = Command::new("rsync")
        .args(["-n", "-a", "-8", "--itemize-changes", "--out-format=%n"])
        .args(ex.rsync_args())
        .arg(format!("{}/", src.display()))
        .arg(format!("{}/", dst.display()))
        .output()
        .expect("the differential test needs rsync on PATH");
    assert!(out.status.success(), "rsync failed: {out:?}");
    let sent: BTreeSet<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| l.trim_end_matches('/').to_string())
        .filter(|l| !l.is_empty() && l != ".")
        .collect();
    all()
        .into_iter()
        .map(|(p, _)| p)
        .filter(|p| !sent.contains(p))
        .collect()
}

fn agree(ex: &Excludes, label: &str) -> BTreeSet<String> {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir(&src).unwrap();
    build(&src);
    let (a, b) = (ours(ex), theirs(&src, ex));
    assert_eq!(
        a,
        b,
        "{label}: replicant (left) and {} (right) disagree\nrules: {:?}",
        flavour(),
        ex.rsync_args()
    );
    a
}

fn set(paths: &[&str]) -> BTreeSet<String> {
    paths.iter().map(|p| p.to_string()).collect()
}

#[test]
fn each_pattern_class_agrees_with_rsync() {
    let cases: &[(&str, &[&str])] = &[
        (
            "target",
            &[
                "target",
                "target/debug.o",
                "app/target",
                "app/target/x",
                "app/deep/er/target",
            ],
        ),
        ("*.log", &["app/notes.log"]),
        ("build/", &["build", "build/out"]),
        ("linkdir/", &[]),
        ("/top", &["top"]),
        ("a/**/b", &["a/b", "a/x/b", "a/x/b/f", "a/x/y/b"]),
        ("agents/state", &["agents/state", "agents/state/db"]),
        (
            "**/agents/state",
            &[
                "agents/state",
                "agents/state/db",
                ".sovran/agents/state",
                ".sovran/agents/state/db",
            ],
        ),
        ("src/*.rs", &["src/main.rs"]),
        ("**/gen/*.c", &["gen/a.c", "p/gen/b.c"]),
        ("cache/**", &["cache/one", "cache/sub", "cache/sub/two"]),
        ("v[0-9]", &["v1", "v2"]),
        ("**/x/**/y", &["a/x/y", "a/x/y/b", "q/x/y", "q/x/m/n/y"]),
        ("\\*lit", &["*lit"]),
        ("sp ace", &["sp ace"]),
        ("\\{brace\\}", &["{brace}"]),
        ("we\\\\ird", &["we\\ird"]),
    ];
    for (pat, want) in cases {
        let ex = Excludes::new(&[pat]).unwrap();
        assert_eq!(agree(&ex, pat), set(want), "{pat}: the expected set");
    }
}

#[test]
fn builtins_agree_with_rsync_at_any_depth() {
    let ex = Excludes::new(BUILTIN).unwrap();
    let got = agree(&ex, "BUILTIN");
    assert_eq!(
        got,
        set(&[
            ".git/index.lock",
            ".git/refs/heads/main.lock",
            ".git/worktrees",
            ".git/worktrees/wt",
            ".git/worktrees/wt/HEAD",
            "nested/.git/HEAD.lock",
        ])
    );
}

#[test]
fn exact_paths_agree_with_rsync_whatever_their_characters() {
    let mut ex = Excludes::default();
    for (p, d) in [
        ("we\\ird", false),
        ("st*r", false),
        ("sp ace", false),
        ("{brace}", false),
        ("a/x", true),
        ("q/x/m", true),
        ("top", true), // a file: a dir-only rule must leave it alone
    ] {
        ex.push_exact(p, d);
    }
    let got = agree(&ex, "push_exact");
    assert_eq!(
        got,
        set(&[
            "we\\ird",
            "st*r",
            "sp ace",
            "{brace}",
            "a/x",
            "a/x/b",
            "a/x/b/f",
            "a/x/y",
            "a/x/y/b",
            "q/x/m",
            "q/x/m/n",
            "q/x/m/n/y",
        ])
    );
}

#[test]
fn a_whole_root_list_agrees_with_rsync() {
    let mut ex = Excludes::for_root(&[
        "target".into(),
        "*.log".into(),
        "build/".into(),
        "**/agents/state".into(),
        "**/gen/*.c".into(),
        "cache/**".into(),
    ])
    .unwrap();
    ex.push_exact("nested", true);
    ex.push_exact("st*r", false);
    agree(&ex, "root list");
}

#[test]
fn everything_and_nothing_agree_with_rsync() {
    assert!(agree(&Excludes::default(), "empty").is_empty());
    let every = agree(&Excludes::new(&["**"]).unwrap(), "**");
    assert_eq!(every.len(), all().len());
}

/// The walker that feeds guardrail 4 sees the same tree rsync does.
#[test]
fn all_names_matches_rsync_on_the_same_list() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir(&src).unwrap();
    build(&src);
    let ex = Excludes::for_root(&["target".into(), "**/gen/*.c".into(), "a/**/b".into()]).unwrap();
    let walked: BTreeSet<String> = crate::repos::all_names(&src, &ex)
        .unwrap()
        .into_iter()
        .collect();
    let dropped = theirs(&src, &ex);
    let kept: BTreeSet<String> = all()
        .into_iter()
        .map(|(p, _)| p)
        .filter(|p| !dropped.contains(p))
        .collect();
    assert_eq!(walked, kept, "{}", flavour());
}

#[path = "rsync_diff_units_tests.rs"]
mod units;

#[path = "rsync_diff_class_tests.rs"]
mod class;
