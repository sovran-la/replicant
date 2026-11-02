//! The write set, held to the code: every place outside the tests that
//! starts a process or writes to the filesystem, counted per file, against a
//! list that says which part of the write set each one is. A new spawn or
//! write anywhere fails this test until it is added here, with its reason.
//!
//! The write set (docs/v0.2-prep-notes.md, "Apply"): synced paths only
//! through rsync; markers, baselines, contacts and the journal under the
//! state and config dirs; private scratch files under the config dir;
//! home's own identity files and fleet.toml. The v0.1 review checked this by
//! hand.

use std::collections::BTreeMap;
use std::path::Path;

/// What counts as a spawn or a write.
const PATTERNS: &[&str] = &[
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
];

#[path = "write_set_allowed.rs"]
mod allowed;
use allowed::ALLOWED;

fn is_test_file(rel: &str) -> bool {
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
        )
}

/// The file's code before its inline test module, if it has one.
fn code(text: &str) -> &str {
    match text.find("#[cfg(test)]\nmod tests {") {
        Some(i) => &text[..i],
        None => text,
    }
}

fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<(String, &'static str), usize>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            walk(&p, root, out);
            continue;
        }
        let rel = p.strip_prefix(root).unwrap().display().to_string();
        if !rel.ends_with(".rs") || is_test_file(&rel) {
            continue;
        }
        let text = std::fs::read_to_string(&p).unwrap();
        for line in code(&text).lines() {
            let t = line.trim_start();
            if t.starts_with("//") {
                continue;
            }
            for pat in PATTERNS {
                let n = t.matches(pat).count();
                if n > 0 {
                    *out.entry((rel.clone(), *pat)).or_default() += n;
                }
            }
        }
    }
}

fn found() -> BTreeMap<(String, &'static str), usize> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = BTreeMap::new();
    walk(&root, &root, &mut out);
    out
}

#[test]
fn every_spawn_and_write_is_on_the_list() {
    let found = found();
    let allowed: BTreeMap<(String, &str), usize> = ALLOWED
        .iter()
        .map(|(f, p, n, _)| ((f.to_string(), *p), *n))
        .collect();
    let mut wrong = Vec::new();
    for (k, n) in &found {
        match allowed.get(&(k.0.clone(), k.1)) {
            Some(a) if a == n => {}
            Some(a) => wrong.push(format!("{} `{}`: {n} found, {a} allowed", k.0, k.1)),
            None => wrong.push(format!("{} `{}`: {n} found, not on the list", k.0, k.1)),
        }
    }
    for (k, a) in &allowed {
        if !found.contains_key(&(k.0.clone(), k.1)) {
            wrong.push(format!(
                "{} `{}`: {a} allowed, none found (stale entry)",
                k.0, k.1
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "the write set moved:\n{}",
        wrong.join("\n")
    );
}

#[test]
fn the_check_sees_a_write_it_was_not_told_about() {
    // The scanner itself: a write in code is counted, one in a comment or an
    // inline test module is not.
    let text = "fn a() { std::fs::write(p, x); }\n// std::fs::write(q, y);\n\
                #[cfg(test)]\nmod tests {\n fn t() { std::fs::write(r, z); }\n}\n";
    let lines: Vec<&str> = code(text)
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect();
    let n: usize = lines.iter().map(|l| l.matches("fs::write(").count()).sum();
    assert_eq!(n, 1);
    assert!(is_test_file("plan/apply_tests.rs") && is_test_file("plan/tests_more.rs"));
    assert!(!is_test_file("plan/apply.rs"));
}
