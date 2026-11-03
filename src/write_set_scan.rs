//! The write-set scanner: what counts as a write, and what would hide one
//! from a text match (review A4). Used by `write_set_tests.rs`.

use std::collections::BTreeMap;

/// What counts as a spawn, an rsync run or a write.
pub(super) const PATTERNS: &[&str] = &[
    "Command::new(",
    "fs::write(",
    "create_dir_all(",
    "create_dir(",
    "remove_file(",
    "remove_dir_all(",
    "remove_dir(",
    "fs::rename(",
    "OpenOptions::new(",
    "File::options(",
    "File::create(",
    "set_permissions(",
    "fs::copy(",
    "hard_link(",
    "fs::symlink(",
    "libc::mkstemp(",
    "libc::mkdtemp(",
    "DirBuilder::new(",
    "write_atomic(",
    ".put(",
    ".delete(",
    "append_raw(",
    "set_modified(",
    // A file's times (the native receiver gives each its source's mtime).
    "set_times(",
    // rsync's entry points (review A4): every run is a potential write.
    // Each in both spellings (re-review R3): `transfer.apply(r)` and
    // `Transfer::apply(&transfer, r)`; `runner.run(a)` and
    // `Runner::run(&runner, a)`. `::run(` matches every other `X::run(`
    // too, so those are listed, each with what it is. `copy_listed(` and
    // `delete_listed(` already match every spelling, `rsync::copy_listed(`
    // and `Self::delete_listed(` included.
    "child::run(",
    ".apply(",
    "::apply(",
    ".run(",
    "::run(",
    "copy_listed(",
    "delete_listed(",
    // libc calls that write (review A4).
    "libc::unlink(",
    "libc::unlinkat(",
    "libc::rename(",
    "libc::renameat(",
    "libc::open(",
    "libc::openat(",
    "libc::mkdir(",
    "libc::mkdirat(",
    "libc::symlink(",
    "libc::symlinkat(",
    "libc::link(",
    "libc::chmod(",
    "libc::fchmod(",
    "libc::fchmodat(",
    "libc::rmdir(",
    "libc::truncate(",
    "libc::ftruncate(",
    "libc::write(",
    "libc::utimensat(",
    "libc::futimens(",
    "libc::utimes(",
];

pub(super) fn is_test_file(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name.ends_with("_tests.rs")
        || name == "tests.rs"
        || name.starts_with("tests_")
        || matches!(
            name,
            "testutil.rs"
                | "test_certs.rs"
                | "rsync_diff_fixture.rs"
                | "write_set_tests.rs"
                | "write_set_allowed.rs"
                | "write_set_spawns.rs"
                | "write_set_scan.rs"
        )
}

/// The file's code before its inline test module, if it has one.
fn code(text: &str) -> &str {
    match text.find("#[cfg(test)]\nmod tests {") {
        Some(i) => &text[..i],
        None => text,
    }
}

/// Its lines that are not comments.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    code(text)
        .lines()
        .map(str::trim_start)
        .filter(|t| !t.starts_with("//"))
}

/// How many times each pattern occurs in a file's code.
pub(super) fn count(text: &str) -> BTreeMap<&'static str, usize> {
    let mut out = BTreeMap::new();
    for t in lines(text) {
        for pat in PATTERNS {
            let n = t.matches(pat).count();
            if n > 0 {
                *out.entry(*pat).or_default() += n;
            }
        }
    }
    out
}

/// Every `use` statement in a file's code, joined onto one line.
fn uses(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    for t in lines(text) {
        let starts = t.starts_with("use ") || (t.starts_with("pub") && t.contains(" use "));
        if cur.is_none() && starts {
            cur = Some(String::new());
        }
        if let Some(c) = cur.as_mut() {
            c.push_str(t);
            c.push(' ');
            if t.contains(';') {
                out.extend(cur.take());
            }
        }
    }
    out
}

/// What a file does that would hide a write from [`count`].
pub(super) fn forbidden(text: &str) -> Vec<String> {
    let mut out: Vec<String> = lines(text)
        .filter(|t| t.contains("Command as"))
        .map(|t| format!("a `Command as` alias: {t}"))
        .collect();
    for u in uses(text) {
        let why = if u.contains("std::process::Command as") {
            Some("`std::process::Command as`")
        } else if fs_functions(&u) {
            Some(
                "a function import from std::fs or std::os::unix::fs (spell the call \
                 `std::fs::…` or `fs::…`)",
            )
        } else if u.contains("libc::") {
            Some("an import from libc (spell the call `libc::…`)")
        } else if u.contains("child::run") || (u.contains("child::{") && u.contains("run")) {
            Some("an import of rsync's child::run")
        } else if u.split(" as ").skip(1).any(|rest| !rest.starts_with('_')) {
            Some("a renamed import (`use … as …`)")
        } else {
            None
        };
        out.extend(why.map(|w| format!("{w}: {}", u.trim_end())));
    }
    out
}

/// Whether a joined `use` imports a function (any lowercase name other than
/// `self`, or a `*` glob) from under an `fs::` segment, at any depth of
/// `{…}` nesting (re-review R3): `std::fs::write`, `std::{fs::write}`,
/// `std::os::unix::{fs::symlink}`. Types and traits (`File`,
/// `PermissionsExt`) are fine: their writes are spelled `Type::fn(`.
fn fs_functions(u: &str) -> bool {
    let mut names = Vec::new();
    for (i, _) in u.match_indices("fs::") {
        let segment_starts = u[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        if segment_starts {
            let rest = &u[i + "fs::".len()..];
            leaves(&rest[..tree_end(rest)], &mut names);
        }
    }
    names
        .iter()
        .any(|n| *n != "self" && !n.starts_with(|c: char| c.is_uppercase()))
}

/// Where the use-tree at the start of `s` ends: the first `,`, `;` or `}`
/// outside its own braces.
fn tree_end(s: &str) -> usize {
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' if depth > 0 => depth -= 1,
            ',' | ';' | '}' if depth == 0 => return i,
            _ => {}
        }
    }
    s.len()
}

/// The names a use-tree imports: the last segment of each path in it, with
/// any `as` rename dropped.
fn leaves<'a>(tree: &'a str, out: &mut Vec<&'a str>) {
    let tree = tree.trim();
    let Some(open) = tree.find('{') else {
        let last = tree.rsplit("::").next().unwrap_or("");
        out.extend(last.split_whitespace().next());
        return;
    };
    let mut rest = &tree[open + 1..];
    loop {
        let end = tree_end(rest);
        leaves(&rest[..end], out);
        match rest[end..].chars().next() {
            Some(',') => rest = &rest[end + 1..],
            _ => break,
        }
    }
}
