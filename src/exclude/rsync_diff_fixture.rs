//! The differential tests' fixture tree.

use std::collections::BTreeSet;
use std::path::Path;

/// The fixture: `x/` is a directory, `@x` a symlink to `.`, anything else a
/// file. Names repeat at several depths on purpose.
pub(super) const TREE: &[&str] = &[
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

pub(super) fn build(root: &Path) {
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

pub(super) fn all() -> BTreeSet<(String, bool)> {
    TREE.iter()
        .map(|p| {
            let p = p.trim_start_matches('@');
            (p.trim_end_matches('/').to_string(), p.ends_with('/'))
        })
        .collect()
}
