use super::*;
use crate::native::tests_fixture::{Fx, manifest, run, tree};
use crate::testutil::{self, fingerprint, write};

const ACK: Duration = Duration::from_secs(20);

fn none() -> Abort {
    Abort::default()
}

/// Counts only grow, and the last equals the totals.
fn monotonic_to(seen: &[Counts], files: u64, bytes: u64) {
    assert!(!seen.is_empty(), "progress was reported");
    for w in seen.windows(2) {
        assert!(
            w[1].files_done >= w[0].files_done && w[1].bytes_done >= w[0].bytes_done,
            "{seen:?}"
        );
    }
    let last = seen.last().unwrap();
    assert_eq!(
        (last.files_done, last.bytes_done),
        (files, bytes),
        "{seen:?}"
    );
    assert_eq!((last.files_total, last.bytes_total), (files, bytes));
}

#[test]
fn a_push_makes_home_the_leafs_tree_and_leaves_the_leaf_alone() {
    for streams in [1, 4] {
        let fx = Fx::new();
        tree(&fx.leaf(), 3);
        write(&fx.home_work().join("only-home.txt"), "deleted by the push");
        write(
            &fx.home_work().join("small/000/f0.txt"),
            "home's older copy",
        );
        write(
            &fx.home_work().join("target/home-junk"),
            "excluded on home: stays",
        );
        let before = fingerprint(&fx.leaf());
        let lease = fx.lease();
        let push = fx.begin_push(lease);
        let t = fx.transfer(Direction::Push);
        let (d, ran, seen) = run(&fx.link(), &t, lease, streams, &none(), ACK);

        assert!(ran.ok(), "{streams}: {ran:?}");
        assert_eq!(ran.itemized, d.itemized, "all of the dry run, nothing else");
        assert_eq!(
            manifest(&fx.home_work()),
            manifest(&fx.leaf()),
            "metadata to the ns"
        );
        assert_eq!(
            std::fs::read(fx.home_work().join("big/blob2.bin")).unwrap(),
            std::fs::read(fx.leaf().join("big/blob2.bin")).unwrap()
        );
        assert_eq!(fingerprint(&fx.leaf()), before, "the source is untouched");
        assert!(fx.home_work().join("target/home-junk").exists());
        let (files, bytes) = d.changes.totals();
        monotonic_to(&seen, files, bytes);
        // Home kept what the push replaced and deleted.
        let run = fx.bed.home_dirs.backup().join(&push).join("run-1");
        assert_eq!(
            std::fs::read_to_string(run.join("only-home.txt")).unwrap(),
            "deleted by the push"
        );
        assert_eq!(
            std::fs::read_to_string(run.join("small/000/f0.txt")).unwrap(),
            "home's older copy"
        );
        fx.release(lease);
    }
}

#[test]
fn a_pull_makes_the_leaf_homes_tree_and_leaves_home_alone() {
    let fx = Fx::new();
    tree(&fx.home_work(), 3);
    let before = fingerprint(&fx.home_work());
    let lease = fx.lease();
    let t = fx.transfer(Direction::Pull);
    let mut targets = Vec::new();
    for streams in [1, 4] {
        let _ = std::fs::remove_dir_all(fx.leaf());
        write(&fx.leaf().join("only-leaf.txt"), "deleted by the pull");
        write(&fx.leaf().join("target/leaf-junk"), "excluded: stays");
        let (d, ran, seen) = run(&fx.link(), &t, lease, streams, &none(), ACK);
        assert!(ran.ok(), "{ran:?}");
        assert_eq!(ran.itemized, d.itemized);
        assert_eq!(manifest(&fx.leaf()), manifest(&fx.home_work()));
        assert_eq!(
            fingerprint(&fx.home_work()),
            before,
            "the source is untouched"
        );
        assert!(fx.leaf().join("target/leaf-junk").exists());
        let (files, bytes) = d.changes.totals();
        monotonic_to(&seen, files, bytes);
        targets.push(manifest(&fx.leaf()));
    }
    // One stream and four end in the same tree (mtimes included).
    assert_eq!(targets[0], targets[1]);
    fx.release(lease);
}

#[test]
fn a_second_transfer_has_nothing_to_do() {
    let fx = Fx::new();
    tree(&fx.home_work(), 1);
    let lease = fx.lease();
    let t = fx.transfer(Direction::Pull);
    assert!(run(&fx.link(), &t, lease, 4, &none(), ACK).1.ok());
    let (d, ran, seen) = run(&fx.link(), &t, lease, 4, &none(), ACK);
    assert!(
        d.changes.is_empty() && ran.ok() && ran.itemized.changes.is_empty(),
        "{d:?}"
    );
    assert!(seen.is_empty(), "nothing sent, nothing reported");
}

#[test]
fn kind_changes_both_ways() {
    let fx = Fx::new();
    let (l, h) = (fx.leaf(), fx.home_work());
    write(&l.join("f2d/inner"), "now a directory");
    write(&l.join("d2f"), "now a file");
    std::os::unix::fs::symlink("d2f", l.join("f2l")).unwrap();
    write(&h.join("f2d"), "was a file");
    write(&h.join("d2f/inner"), "was a directory");
    write(&h.join("f2l"), "was a file");
    let lease = fx.lease();
    fx.begin_push(lease);
    let (_, ran, _) = run(
        &fx.link(),
        &fx.transfer(Direction::Push),
        lease,
        2,
        &none(),
        ACK,
    );
    assert!(ran.ok(), "{ran:?}");
    assert_eq!(manifest(&h), manifest(&l));
    // And back: home changes kinds again, the leaf pulls.
    std::fs::remove_dir_all(h.join("f2d")).unwrap();
    write(&h.join("f2d"), "a file again");
    let (_, ran, _) = run(
        &fx.link(),
        &fx.transfer(Direction::Pull),
        lease,
        2,
        &none(),
        ACK,
    );
    assert!(ran.ok(), "{ran:?}");
    assert_eq!(manifest(&l), manifest(&h));
}

#[test]
fn a_deletion_carried_out_removes_the_target_and_spares_what_does_not_sync() {
    let fx = Fx::new();
    tree(&fx.leaf(), 0);
    let lease = fx.lease();
    let mut t = fx.transfer(Direction::Pull);
    t.source_absent = true;
    let (d, ran, _) = run(&fx.link(), &t, lease, 4, &none(), ACK);
    assert!(ran.ok(), "{ran:?}");
    assert!(d.itemized.deleted() > 100);
    assert_eq!(
        manifest(&fx.leaf())
            .entries
            .iter()
            .map(|e| e.rel.as_str())
            .collect::<Vec<_>>(),
        Vec::<&str>::new()
    );
    assert!(
        fx.leaf().join("target/junk").exists(),
        "excluded: never deleted"
    );

    // Pushed: home's copy goes, into the push's backup.
    tree(&fx.home_work(), 0);
    let push = fx.begin_push(lease);
    let mut t = fx.transfer(Direction::Push);
    t.source_absent = true;
    let (_, ran, _) = run(&fx.link(), &t, lease, 4, &none(), ACK);
    assert!(ran.ok(), "{ran:?}");
    assert!(manifest(&fx.home_work()).entries.is_empty());
    let kept = fx
        .bed
        .home_dirs
        .backup()
        .join(push)
        .join("run-1/nested/a/b/c/deep.txt");
    assert_eq!(std::fs::read_to_string(kept).unwrap(), "deep");
}

#[test]
fn a_source_that_changed_since_the_dry_run_fails_verification() {
    let fx = Fx::new();
    write(&fx.home_work().join("a"), "x");
    let lease = fx.lease();
    let t = fx.transfer(Direction::Pull);
    let seen = |_: Counts| {};
    let link = fx.link();
    let abort = Abort::default();
    let env = Env {
        link: &link,
        me: "leaf",
        lease,
        abort: &abort,
        streams: 2,
        ack_timeout: ACK,
        progress: &seen,
    };
    let d = t.dry_run(&env).unwrap();
    write(
        &fx.home_work().join("arrived-after"),
        "not in the change list",
    );
    let ran = t.apply(&env, &d);
    let why = ran.failure.clone().unwrap();
    assert!(
        why.contains("still differ") && why.contains("arrived-after"),
        "{why}"
    );
    assert!(!ran.stopped);
    assert!(
        fx.leaf().join("a").exists(),
        "what was planned arrived whole"
    );
    // The next run picks it up.
    let d = t.dry_run(&env).unwrap();
    assert!(t.apply(&env, &d).ok());
    testutil::write(&fx.leaf().join("unrelated"), "");
}

#[test]
fn work_items_put_big_files_first_and_batch_the_rest() {
    use crate::native::workers::{Item, items};
    let f = |rel: &str, size| crate::native::Entry {
        rel: rel.into(),
        kind: crate::native::EntryKind::File,
        size,
        mtime_ns: 0,
        mode: 0o644,
        link: None,
    };
    let send = vec![
        f("a", 10),
        f("b", 5 << 20),
        f("c", 70_000),
        f("d", 20),
        f("e", 1 << 30),
    ];
    assert_eq!(
        items(&send),
        [
            Item::Big("e".into()),
            Item::Big("b".into()),
            Item::Big("c".into()),
            Item::Batch(vec!["a".into(), "d".into()])
        ]
    );
    let many: Vec<_> = (0..5000).map(|i| f(&format!("f{i:04}"), 1)).collect();
    let it = items(&many);
    assert_eq!(it.len(), 3, "batches close at their entry limit");
    let lots: Vec<_> = (0..40).map(|i| f(&format!("m{i:02}"), 60_000)).collect();
    assert!(items(&lots).len() >= 2, "batches close at their byte limit");
}
