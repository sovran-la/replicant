//! The write set, held to the code: every place outside the tests that
//! starts a process, calls into rsync or writes to the filesystem, counted
//! per file, against a list that says which part of the write set each one
//! is. A new spawn, rsync run or write anywhere fails this test until it is
//! added here, with its reason.
//!
//! The scan matches text, so the ways around a text match are forbidden
//! outright (review A4): importing `std::fs` functions (or `{…}` lists of
//! them), a `Command as` alias, any renamed `use … as …`, `libc` item
//! imports, and importing rsync's `child::run`. With those gone, every
//! write is spelled as one of `scan::PATTERNS`.
//!
//! The write set (docs/v0.2-prep-notes.md, "Apply"): synced paths only
//! through rsync; markers, baselines, contacts and the journal under the
//! state and config dirs; private scratch files under the config dir;
//! home's own identity files and fleet.toml. The v0.1 review checked this by
//! hand.

use std::collections::BTreeMap;
use std::path::Path;

#[path = "write_set_scan.rs"]
mod scan;
use scan::{count, forbidden, is_test_file};

#[path = "write_set_allowed.rs"]
mod allowed;
#[path = "write_set_spawns.rs"]
mod spawns;

fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, String>) {
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
        out.insert(rel, std::fs::read_to_string(&p).unwrap());
    }
}

/// Every non-test source file under `src/`, by its path there.
fn sources() -> BTreeMap<String, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = BTreeMap::new();
    walk(&root, &root, &mut out);
    out
}

fn found() -> BTreeMap<(String, &'static str), usize> {
    let mut out = BTreeMap::new();
    for (rel, text) in sources() {
        for (pat, n) in count(&text) {
            out.insert((rel.clone(), pat), n);
        }
    }
    out
}

#[test]
fn every_spawn_and_write_is_on_the_list() {
    let found = found();
    let allowed: BTreeMap<(String, &str), usize> = allowed::ALLOWED
        .iter()
        .chain(spawns::SPAWNS)
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
fn nothing_outside_the_tests_hides_a_write_from_the_scan() {
    let wrong: Vec<String> = sources()
        .into_iter()
        .flat_map(|(rel, text)| {
            forbidden(&text)
                .into_iter()
                .map(move |w| format!("{rel}: {w}"))
        })
        .collect();
    assert!(wrong.is_empty(), "forbidden:\n{}", wrong.join("\n"));
}

#[test]
fn the_check_sees_a_write_it_was_not_told_about() {
    // The scanner itself: a write in code is counted, one in a comment or an
    // inline test module is not.
    let text = "fn a() { std::fs::write(p, x); }\n// std::fs::write(q, y);\n\
                #[cfg(test)]\nmod tests {\n fn t() { std::fs::write(r, z); }\n}\n";
    assert_eq!(count(text).get("fs::write("), Some(&1));
    assert!(is_test_file("plan/apply_tests.rs") && is_test_file("plan/tests_more.rs"));
    assert!(!is_test_file("plan/apply.rs"));
    // The review's mutant: a real transfer in the dry-run path is counted.
    let text = "fn dry() { let _ = transfer.apply(&runner); t.dry_run(&runner) }\n";
    assert_eq!(count(text).get(".apply("), Some(&1));
    for (call, pat) in [
        ("child::run(&p, &a, t, &ab)", "child::run("),
        ("rsync::copy_listed(&r, a, b, &l)", "copy_listed("),
        ("rsync::delete_listed(&r, None, a, &l)", "delete_listed("),
        ("unsafe { libc::unlink(p) }", "libc::unlink("),
        ("unsafe { libc::rename(a, b) }", "libc::rename("),
        ("unsafe { libc::open(p, f) }", "libc::open("),
        ("unsafe { libc::mkdir(p, m) }", "libc::mkdir("),
        ("unsafe { libc::symlink(a, b) }", "libc::symlink("),
        ("unsafe { libc::chmod(p, m) }", "libc::chmod("),
        ("unsafe { libc::rmdir(p) }", "libc::rmdir("),
    ] {
        assert_eq!(
            count(&format!("fn f() {{ {call}; }}\n")).get(pat),
            Some(&1),
            "{call}"
        );
    }
    assert!(count("fn f() { std::fs::read(p); File::open(p); }\n").is_empty());
}

#[test]
fn the_check_refuses_a_write_through_an_alias() {
    // The review's other mutant: an aliased `std::fs::write` is invisible
    // to the count, so the alias itself is refused.
    let mutant = "use std::fs::write as save_bytes;\nfn f() { save_bytes(p, b\"x\"); }\n";
    assert!(count(mutant).is_empty(), "the count cannot see it");
    let why = forbidden(mutant);
    assert_eq!(why.len(), 1, "{why:?}");
    assert!(why[0].contains("std::fs"), "{why:?}");
    for bad in [
        "use std::fs::write;\n",
        "use std::fs::{self, write};\n",
        "use std::fs::{\n    remove_file,\n    write,\n};\n",
        "use std::fs::File as F;\n",
        "use std::process::Command as Cmd;\n",
        "use std::process::{Command as Cmd, Stdio};\n",
        "type C = std::process::Command;\nfn f() { let c = x as Command as y; }\n",
        "use std::os::unix::fs::symlink;\n",
        "use libc::unlink;\n",
        "use libc::{rename, unlink};\n",
        "use super::child::run;\n",
        "use super::child::{run, Abort};\n",
        "use crate::rsync::copy_listed as cp;\n",
        "use crate::rsync::{\n    copy_listed as cp,\n    Runner,\n};\n",
        "pub(crate) use crate::rsync::delete_listed as dl;\n",
    ] {
        assert!(!forbidden(bad).is_empty(), "not refused: {bad}");
    }
    for fine in [
        "use std::fs;\n",
        "use std::fs::File;\n",
        "use std::os::unix::fs::{MetadataExt, PermissionsExt};\n",
        "use std::process::{Command, Stdio};\n",
        "pub use child::{Abort, Stopped};\n",
        "use super::{Itemized, Runner, child, parse};\n",
        "use std::io::Write as _;\n",
        "// use std::fs::write as w;\n",
    ] {
        assert_eq!(forbidden(fine), Vec::<String>::new(), "refused: {fine}");
    }
}
