use super::*;
use crate::repos::discover;
use crate::testutil;
use std::process::Command;

fn h(abs: &Path) -> UnitState {
    repo(abs, &Excludes::default()).unwrap().state
}

/// Move a file's mtime forward by one second, whatever the clock says.
fn bump_mtime(path: &Path) {
    let f = std::fs::File::options().write(true).open(path).unwrap();
    let was = f.metadata().unwrap().modified().unwrap();
    f.set_modified(was + std::time::Duration::from_secs(1))
        .unwrap();
}

/// The documented blind spot, pinned so it is a decision and not a surprise:
/// a rewrite with the same size and the same mtime is invisible to the hash.
#[test]
fn same_size_same_mtime_rewrite_is_invisible_by_design() {
    let tmp = tempfile::tempdir().unwrap();
    let r = testutil::repo(&tmp.path().join("r"));
    testutil::write(&r.join("README"), "one\n");
    let f = std::fs::File::options()
        .write(true)
        .open(r.join("README"))
        .unwrap();
    let t = f.metadata().unwrap().modified().unwrap();
    let before = h(&r);
    testutil::write(&r.join("README"), "two\n");
    f.set_modified(t).unwrap();
    assert_eq!(h(&r), before);
}

#[test]
fn repo_hash_is_stable_and_moves_with_every_kind_of_state() {
    let tmp = tempfile::tempdir().unwrap();
    let r = testutil::repo(&tmp.path().join("r"));
    let mut seen = vec![h(&r)];
    assert_eq!(h(&r), seen[0], "stable across calls");

    let mut step = |label: &str, f: &dyn Fn()| {
        f();
        let now = h(&r);
        assert!(!seen.contains(&now), "{label} did not change the hash");
        seen.push(now);
    };
    step("worktree edit", &|| {
        testutil::write(&r.join("README"), "one\n")
    });
    // Same porcelain line (` .M` with the same blob ids), same size: only
    // the mtime moves. Bumped by a full second so no filesystem's clock
    // granularity can make this flake.
    step("second worktree edit", &|| {
        testutil::write(&r.join("README"), "two\n");
        bump_mtime(&r.join("README"));
    });
    step("worktree size change", &|| {
        testutil::write(&r.join("README"), "three!\n")
    });
    step("stage", &|| {
        testutil::git(&r, &["add", "README"]);
    });
    step("untracked", &|| testutil::write(&r.join("u.txt"), "u\n"));
    step("untracked mtime", &|| {
        testutil::write(&r.join("u.txt"), "v\n");
        bump_mtime(&r.join("u.txt"));
    });
    step("commit", &|| {
        testutil::git(&r, &["add", "-A"]);
        testutil::git(&r, &["commit", "-qm", "c"]);
    });
    step("branch", &|| {
        testutil::git(&r, &["branch", "side"]);
    });
    step("checkout", &|| {
        testutil::git(&r, &["checkout", "-q", "side"]);
    });
    step("stash", &|| {
        testutil::write(&r.join("README"), "stashed\n");
        testutil::git(&r, &["stash", "-q"]);
    });
    step("worktree delete", &|| {
        std::fs::remove_file(r.join("README")).unwrap()
    });
}

#[test]
fn identical_copies_hash_identically() {
    let tmp = tempfile::tempdir().unwrap();
    let a = testutil::repo(&tmp.path().join("a"));
    testutil::write(&a.join("README"), "dirty\n");
    testutil::write(&a.join("new"), "n\n");
    let b = tmp.path().join("b");
    let ok = Command::new("cp")
        .args(["-Rp"])
        .arg(&a)
        .arg(&b)
        .status()
        .unwrap();
    assert!(ok.success());
    assert_eq!(h(&a), h(&b), "the hash is comparable across machines");
}

#[test]
fn excluded_paths_do_not_count() {
    let tmp = tempfile::tempdir().unwrap();
    let r = testutil::repo(&tmp.path().join("r"));
    let ex = Excludes::new(&["node_modules"]);
    let before = repo(&r, &ex).unwrap();
    testutil::write(&r.join("node_modules/pkg/index.js"), "x");
    let after = repo(&r, &ex).unwrap();
    assert_eq!(before.state, after.state);
    assert!(after.dirty.is_empty());
    assert_ne!(h(&r), before.state, "without the exclude it is dirty");
}

#[test]
fn facts_carry_dirty_paths_and_locks() {
    let tmp = tempfile::tempdir().unwrap();
    testutil::tree(tmp.path());
    let f = repo(&tmp.path().join("beta"), &Excludes::default()).unwrap();
    assert_eq!(f.dirty, vec!["README", "new.txt"]);
    assert_eq!(f.locks, vec![".git/index.lock"]);
    assert_eq!(f.head, "ref: refs/heads/main");
    let clean = repo(&tmp.path().join("alpha"), &Excludes::default()).unwrap();
    assert!(clean.dirty.is_empty() && clean.locks.is_empty());
}

#[test]
fn loose_manifest_hash() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(loose(&[]), UnitState::Empty);
    testutil::write(&tmp.path().join("a.txt"), "a");
    let ex = Excludes::default();
    let one = loose(&discover(tmp.path(), &ex).unwrap().loose);
    assert!(!one.is_empty());
    assert_eq!(one, loose(&discover(tmp.path(), &ex).unwrap().loose));
    testutil::write(&tmp.path().join("a.txt"), "ab");
    let two = loose(&discover(tmp.path(), &ex).unwrap().loose);
    assert_ne!(one, two, "size change");
    std::os::unix::fs::symlink("a.txt", tmp.path().join("l")).unwrap();
    assert_ne!(
        two,
        loose(&discover(tmp.path(), &ex).unwrap().loose),
        "new symlink"
    );
}

#[test]
fn short_display() {
    assert_eq!(UnitState::Empty.short(), "empty");
    assert_eq!(
        UnitState::Present("0123456789abcdef".into()).short(),
        "0123456789ab"
    );
}

#[test]
fn parallel_map_preserves_order_and_handles_edges() {
    let items: Vec<u32> = (0..100).collect();
    assert_eq!(
        parallel_map(&items, 7, |x| x * 2),
        items.iter().map(|x| x * 2).collect::<Vec<_>>()
    );
    let none: Vec<u32> = vec![];
    assert!(parallel_map(&none, 4, |x| *x).is_empty());
    assert_eq!(
        parallel_map(&[1], 0, |x| *x),
        vec![1],
        "zero workers still runs"
    );
}

#[test]
fn short_of_a_non_ascii_value_from_the_wire_does_not_panic() {
    // 11 ASCII bytes then a two-byte char straddling byte 12.
    let s = UnitState::Present("0123456789aé-tail".into());
    assert_eq!(
        s.short(),
        "0123456789aé-tail",
        "no boundary at 12: shown whole"
    );
    let s = UnitState::Present("ééééééé".into());
    assert_eq!(s.short(), "éééééé", "boundary at 12: cut there");
    assert_eq!(UnitState::Present("abc".into()).short(), "abc");
}
