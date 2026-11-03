//! Home is also the backup, natively: a push renames what it replaces or
//! deletes into its run's directory, and `rollback` puts home back exactly,
//! also for a push whose leaf died before it reported anything.

use super::*;
use crate::native::tests_fixture::{Fault, Faulty, Fx, manifest, run, tree};
use crate::testutil::{fingerprint, write};

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

#[test]
fn a_native_push_rolled_back_puts_home_back_exactly() {
    let fx = Fx::new();
    diverge(&fx);
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
        "links and nanoseconds too"
    );
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
