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
    // rsync's entry points (review A4): every run is a potential write.
    "child::run(",
    ".apply(",
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
        // Whether `prefix` is followed by a function (a lowercase name other
        // than `self`), alone or in a `{…}` list. Types and traits are fine:
        // their writes are spelled `Type::fn(`.
        let item_after = |prefix: &str| {
            u.split(prefix).skip(1).any(|rest| {
                let items = match rest.strip_prefix('{') {
                    Some(list) => list.split('}').next().unwrap_or(""),
                    None => rest.split([';', ' ']).next().unwrap_or(""),
                };
                items
                    .split(',')
                    .map(|i| i.trim().split(' ').next().unwrap_or(""))
                    .any(|i| i != "self" && i.starts_with(|c: char| c.is_lowercase()))
            })
        };
        let why = if u.contains("std::process::Command as") {
            Some("`std::process::Command as`")
        } else if u.contains("std::fs::{") || item_after("std::fs::") {
            Some("an import from std::fs (spell the call `std::fs::…` or `fs::…`)")
        } else if item_after("std::os::unix::fs::") {
            Some("a function import from std::os::unix::fs")
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
