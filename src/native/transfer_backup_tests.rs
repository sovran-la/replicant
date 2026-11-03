//! Home is also the backup, natively: a push renames what it replaces or
//! deletes into its run's directory, and `rollback` puts home back exactly,
//! also for a push whose leaf died before it reported anything.

use super::*;
use crate::native::tests_fixture::{Fault, Faulty, Fx, manifest, run, tree};
use crate::testutil::{fingerprint, write};
use std::os::unix::fs::PermissionsExt;

type Files = Vec<(String, (Vec<u8>, u32, std::time::SystemTime))>;

/// Files with bytes, mode and mtime; directories by name.
fn print(root: &std::path::Path) -> (Files, Vec<String>) {
    let all = fingerprint(root);
    let files = all
        .iter()
        .filter(|(k, _)| !k.ends_with('/'))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let dirs = all.keys().filter(|k| k.ends_with('/')).cloned().collect();
    (files, dirs)
}

fn lines(it: &Itemized) -> Vec<String> {
    it.changes
        .iter()
        .map(|c| {
            let k = match c.kind {
                ChangeKind::Create => '+',
                ChangeKind::Update => '~',
                ChangeKind::Attr => 'a',
                ChangeKind::Delete => '-',
            };
            format!("{k} {}{}", c.path, if c.is_dir { "/" } else { "" })
        })
        .collect()
}

fn diverge(fx: &Fx) {
    tree(&fx.home_work(), 2);
    write(
        &fx.home_work().join("only-home/deep.txt"),
        "home's, deleted by the push",
    );
    std::thread::sleep(Duration::from_millis(20));
    tree(&fx.leaf(), 2);
    write(&fx.leaf().join("only-leaf.txt"), "created by the push");
}

/// What the push does to directories, as the review's p10-p14 had them, and
/// a mode change on a directory a restored file goes back into.
fn diverge_dirs(fx: &Fx) {
    let chmod = |p: &std::path::Path, m| {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap()
    };
    let (h, l) = (fx.home_work(), fx.leaf());
    // p10: directory to file.
    write(&h.join("dir-to-file/b/x"), "home's");
    chmod(&h.join("dir-to-file/b"), 0o750);
    write(&l.join("dir-to-file"), "a file now");
    // p11: file to directory.
    write(&h.join("file-to-dir"), "home's");
    write(&l.join("file-to-dir/b/x"), "a directory now");
    // p12: a private directory, deleted with its file.
    write(&h.join("priv/secret"), "home's");
    chmod(&h.join("priv"), 0o700);
    // p13: an empty directory, deleted.
    std::fs::create_dir_all(h.join("empty-gone/inner")).unwrap();
    // p14: only the mode changes.
    write(&h.join("modes/same"), "same");
    write(&l.join("modes/same"), "same");
    crate::native::manifest::tests::set_mtime(&h.join("modes/same"), 1_650_000_000, 7);
    crate::native::manifest::tests::set_mtime(&l.join("modes/same"), 1_650_000_000, 7);
    chmod(&h.join("modes"), 0o700);
    chmod(&l.join("modes"), 0o755);
    // Pushed read-only, with its file replaced: the file must get back in.
    write(&h.join("ro/f"), "home's");
    write(&l.join("ro/f"), "the leaf's");
    chmod(&l.join("ro"), 0o555);
}

#[test]
fn a_native_push_rolled_back_puts_home_back_exactly() {
    let fx = Fx::new();
    diverge(&fx);
    diverge_dirs(&fx);
    let (before, before_m) = (print(&fx.home_work()), manifest(&fx.home_work()));
    let lease = fx.lease();
    let id = fx.begin_push(lease);
    let t = fx.transfer(Direction::Push);
    let (_, ran, _) = run(
        &fx.link(),
        &t,
        lease,
        4,
        &Abort::default(),
        Duration::from_secs(20),
    );
    assert!(ran.ok(), "{ran:?}");
    assert_eq!(manifest(&fx.home_work()), manifest(&fx.leaf()));
    let settled = fx.link().call(HomeRequest::Settle {
        lease,
        rel: "work".into(),
        direction: Some(Direction::Push),
        detail: "push ok".into(),
        changes: lines(&ran.itemized),
    });
    assert!(settled.is_ok(), "{settled:?}");
    fx.release(lease);

    let push = &fx.home.pushes().unwrap()[0];
    assert!(
        push.runs[0]
            .backed_up
            .contains(&"only-home/deep.txt".to_string()),
        "{:?}",
        push.runs[0].backed_up
    );
    let r = fx.home.rollback(&id).unwrap();
    assert!(r.unbacked.is_empty() && r.unreported.is_empty(), "{r:?}");
    assert_eq!(
        print(&fx.home_work()),
        before,
        "bytes, modes, mtimes, directories"
    );
    assert_eq!(
        manifest(&fx.home_work()),
        before_m,
        "links, nanoseconds and directories' modes too"
    );
    // The tempdir cleans up after a read-only directory only if it can.
    std::fs::set_permissions(fx.leaf().join("ro"), std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn a_run_that_never_reported_still_gets_its_directories_back() {
    let fx = Fx::new();
    let chmod = |p: &std::path::Path, m| {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap()
    };
    let (h, l) = (fx.home_work(), fx.leaf());
    write(&h.join("kind/b/x"), "home's");
    chmod(&h.join("kind/b"), 0o750);
    write(&l.join("kind"), "a file now");
    std::fs::create_dir_all(h.join("priv")).unwrap();
    chmod(&h.join("priv"), 0o700);
    std::fs::create_dir_all(l.join("made-by-the-run/inner")).unwrap();
    let before = manifest(&h);
    let lease = fx.lease();
    let id = fx.begin_push(lease);
    let t = fx.transfer(Direction::Push);
    let ack = Duration::from_secs(20);
    let (_, ran, _) = run(&fx.link(), &t, lease, 1, &Abort::default(), ack);
    assert!(ran.ok(), "{ran:?}");
    // No Settle: the leaf died before it said what it did.
    fx.release(lease);
    let r = fx.home.rollback(&id).unwrap();
    assert_eq!(r.unreported, [1], "{r:?}");
    assert_eq!(manifest(&fx.home_work()), before);
}

#[test]
fn a_push_whose_leaf_died_mid_unit_is_still_rolled_back_from_its_directory() {
    let fx = Fx::new();
    diverge(&fx);
    let before = print(&fx.home_work());
    let lease = fx.lease();
    let id = fx.begin_push(lease);
    let t = fx.transfer(Direction::Push);
    let link = Faulty::new(fx.link(), Fault::CutAfter(0, 3_000_000));
    let (_, ran, _) = run(
        &link,
        &t,
        lease,
        1,
        &Abort::default(),
        Duration::from_secs(20),
    );
    assert!(!ran.ok());
    // No Interrupted, no Settle: the leaf is gone. Home's queue frees up.
    fx.release(lease);
    let r = fx.home.rollback(&id).unwrap();
    assert_eq!(r.unreported, [1], "{r:?}");
    let now = print(&fx.home_work());
    for (path, was) in &before.0 {
        let back = now.0.iter().find(|(p, _)| p == path).map(|(_, v)| v);
        assert_eq!(back, Some(was), "{path} restored from the run's directory");
    }
}

/// A single-file unit a push created, rolled back: gone, with nothing in its
/// place. (A rollback has no directories to bring back here; opening a receiver
/// on the unit's path anyway would make one.)
#[test]
fn rolling_back_a_single_file_the_push_created_leaves_nothing_there() {
    let fx = Fx::new();
    let (leaf, home) = (
        fx.bed.leaf_hd.join("work/one.txt"),
        fx.home_work().join("one.txt"),
    );
    write(&leaf, "pushed");
    std::fs::create_dir_all(home.parent().unwrap()).unwrap();
    let lease = fx.lease();
    let id = match fx.link().call(HomeRequest::BeginPush {
        lease,
        rel: "work/one.txt".into(),
        source_state: crate::state::UnitState::Empty,
        is_file: true,
    }) {
        Ok(HomeReply::Begun { push }) => push,
        other => panic!("{other:?}"),
    };
    write(&home, "pushed");
    let settled = fx.link().call(HomeRequest::Settle {
        lease,
        rel: "work/one.txt".into(),
        direction: Some(Direction::Push),
        detail: "push ok".into(),
        changes: vec!["+ ".into()],
    });
    assert!(settled.is_ok(), "{settled:?}");
    fx.release(lease);
    fx.home.rollback(&id).unwrap();
    assert!(
        std::fs::symlink_metadata(&home).is_err(),
        "{:?}",
        std::fs::symlink_metadata(&home)
    );
}
