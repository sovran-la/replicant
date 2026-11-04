//! No frame too large, however the tree moved or however long a list is, and
//! no panic that can wedge a transfer (review B3). These must end: each runs
//! under a bound.

use super::*;
use crate::native::tests_fixture::{Fx, bounded, manifest, run};
use crate::testutil::write;
use std::path::Path;

const LIMIT: Duration = Duration::from_secs(240);

/// ~100 bytes of path, as a real tree's are.
fn long(i: usize) -> String {
    format!("list/{:03}/{i:06}-{}", i % 400, "n".repeat(90))
}

/// The directories [`long`]'s paths lie in, under `root`.
fn spread(root: &Path) {
    for i in 0..400 {
        std::fs::create_dir_all(root.join(format!("list/{i:03}"))).unwrap();
    }
}

fn sides(fx: &Fx, dir: Direction) -> (PathBuf, PathBuf) {
    match dir {
        Direction::Push => (fx.leaf(), fx.home_work()),
        Direction::Pull => (fx.home_work(), fx.leaf()),
    }
}

/// p01: the dry run saw a small file, and it is 9 MiB when it is sent.
#[test]
fn a_small_file_grown_past_a_frame_after_the_diff_ends_the_unit_and_no_more() {
    for dir in [Direction::Push, Direction::Pull] {
        bounded(LIMIT, move || {
            let fx = Fx::new();
            let (src, tgt) = sides(&fx, dir);
            write(&src.join("grows"), "small");
            write(&src.join("stays"), "small");
            let lease = fx.lease();
            if dir == Direction::Push {
                fx.begin_push(lease);
            }
            let t = fx.transfer(dir);
            let abort = Abort::default();
            let progress = |_: Counts| {};
            let link = fx.link();
            let env = Env {
                link: &link,
                me: "leaf",
                lease,
                abort: &abort,
                streams: 1,
                ack_timeout: Duration::from_secs(30),
                progress: &progress,
            };
            let d = t.dry_run(&env).unwrap();
            std::fs::write(src.join("grows"), vec![7u8; 9 << 20]).unwrap();
            let ran = t.apply(&env, &d);
            let why = ran.failure.clone().expect("it differs");
            assert!(why.contains("still differ"), "{dir:?}: {why}");
            assert!(ran.stopped.eq(&false));
            assert!(
                !tgt.join("grows").exists(),
                "{dir:?}: never installed from a read past what was listed"
            );
            assert_eq!(std::fs::read(tgt.join("stays")).unwrap(), b"small");
            // The next run sends it as what it now is, a large file.
            let d = t.dry_run(&env).unwrap();
            let ran = t.apply(&env, &d);
            assert!(ran.ok(), "{dir:?}: {ran:?}");
            assert_eq!(std::fs::metadata(tgt.join("grows")).unwrap().len(), 9 << 20);
        });
    }
}

/// Many files, each under `SMALL` when read, that were tiny at the diff: a
/// batch of them is many frames, none too large.
#[test]
fn a_batch_whose_files_all_grew_is_split_not_refused() {
    use crate::native::source::Source;
    let t = tempfile::tempdir().unwrap();
    let body = vec![1u8; 60_000];
    let rels: Vec<String> = (0..200).map(|i| format!("f{i:03}")).collect();
    for r in &rels {
        std::fs::write(t.path().join(r), &body).unwrap();
    }
    let src = Source::open(t.path(), false, None).unwrap();
    let (mut frames, mut sent) = (Vec::new(), 0);
    crate::native::send::batch(&src, &rels, &mut |f, s| {
        frames.push(f.expect("files were read"));
        sent += s.len();
        Ok(())
    })
    .unwrap();
    assert_eq!(sent, 200);
    assert!(frames.len() >= 10, "{}", frames.len());
    let mut got = 0;
    for f in &frames {
        assert!(f.payload.len() <= crate::native::frame::MAX_PAYLOAD);
        got += crate::native::frame::unbatch(f).unwrap().0.len();
    }
    assert_eq!(got, 200);
}

/// p02: a real push deleting 80k files of ~100-byte paths: over 8 MiB of list.
#[test]
fn a_push_deleting_80k_paths_completes() {
    bounded(LIMIT, || {
        let fx = Fx::new();
        write(&fx.leaf().join("kept.txt"), "kept");
        spread(&fx.home_work());
        for i in 0..80_000 {
            std::fs::write(fx.home_work().join(long(i)), "x").unwrap();
        }
        let lease = fx.lease();
        let id = fx.begin_push(lease);
        let t = fx.transfer(Direction::Push);
        let ack = Duration::from_secs(60);
        let (d, ran, _) = run(&fx.link(), &t, lease, 2, &Abort::default(), ack);
        assert!(d.changes.delete.len() > 80_000);
        assert!(ran.ok(), "{ran:?}");
        assert!(!fx.home_work().join("list").exists());
        let kept = fx.bed.home_dirs.backup().join(&id).join("run-1");
        assert!(kept.join(long(79_999)).exists(), "backed up, the last too");
    });
}

/// p03: 90k directories to make, and their modes after. The list is made up,
/// as the review's was (a source tree this size adds nothing but minutes), so
/// verification then finds them extra: every frame was taken by then.
#[test]
fn a_push_making_90k_directories_completes() {
    bounded(LIMIT, || {
        let fx = Fx::new();
        write(&fx.leaf().join("kept.txt"), "kept");
        let lease = fx.lease();
        fx.begin_push(lease);
        let t = fx.transfer(Direction::Push);
        let dir = |rel: String| crate::native::Entry {
            rel,
            kind: crate::native::EntryKind::Dir,
            size: 0,
            mtime_ns: 0,
            mode: 0o755,
            link: None,
        };
        let parents = ["list".to_string()]
            .into_iter()
            .chain((0..400).map(|i| format!("list/{i:03}")));
        let d = Diffed {
            itemized: Itemized::default(),
            changes: crate::native::ChangeList {
                mkdir: parents.chain((0..90_000).map(long)).map(dir).collect(),
                ..Default::default()
            },
            temps: vec![],
            target: Manifest::default(),
        };
        let abort = Abort::default();
        let progress = |_: Counts| {};
        let link = fx.link();
        let env = Env {
            link: &link,
            me: "leaf",
            lease,
            abort: &abort,
            streams: 1,
            ack_timeout: Duration::from_secs(60),
            progress: &progress,
        };
        let ran = t.apply(&env, &d);
        let why = ran.failure.expect("the source has none of them");
        assert!(why.contains("still differ"), "{why}");
        assert!(fx.home_work().join(long(89_999)).is_dir());
    });
}

#[test]
fn a_directory_going_with_what_is_under_it_is_still_backed_up_on_a_push() {
    bounded(LIMIT, || {
        let fx = Fx::new();
        write(&fx.home_work().join("a/b/one.txt"), "home's");
        write(&fx.home_work().join("a/two.txt"), "home's");
        write(&fx.leaf().join("a"), "now a file");
        let lease = fx.lease();
        let id = fx.begin_push(lease);
        let t = fx.transfer(Direction::Push);
        let ack = Duration::from_secs(30);
        let (_, ran, _) = run(&fx.link(), &t, lease, 1, &Abort::default(), ack);
        assert!(ran.ok(), "{ran:?}");
        assert_eq!(manifest(&fx.home_work()), manifest(&fx.leaf()));
        let run1 = fx.bed.home_dirs.backup().join(&id).join("run-1");
        assert!(run1.join("a/b/one.txt").exists() && run1.join("a/two.txt").exists());
    });
}

#[test]
fn a_panic_in_a_worker_is_the_units_failure_and_nothing_hangs() {
    for dir in [Direction::Push, Direction::Pull] {
        bounded(Duration::from_secs(60), move || {
            let fx = Fx::new();
            let (src, _) = sides(&fx, dir);
            write(&src.join("plain.txt"), "plain");
            write(&src.join("panic-me-worker"), "boom");
            let lease = fx.lease();
            if dir == Direction::Push {
                fx.begin_push(lease);
            }
            let t = fx.transfer(dir);
            let ack = Duration::from_secs(30);
            let (_, ran, _) = run(&fx.link(), &t, lease, 2, &Abort::default(), ack);
            let why = ran.failure.expect("not finished");
            assert!(why.contains("panicked"), "{dir:?}: {why}");
            // The queue is free to go on: this lease is released normally.
            fx.release(lease);
        });
    }
}

/// A panic outside any worker (here: opening a connection) ends it the same way.
#[test]
fn a_panic_beside_the_workers_is_the_units_failure_and_nothing_hangs() {
    use crate::plan::{DataLink, HomeLink};
    struct Panics<'a>(crate::testutil::InProcess<'a>);
    impl HomeLink for Panics<'_> {
        fn call(&self, r: HomeRequest) -> crate::Result<HomeReply> {
            self.0.call(r)
        }
        fn open_data(&self, _: &str) -> crate::Result<DataLink<'_>> {
            panic!("forced, opening a data connection")
        }
    }
    for dir in [Direction::Push, Direction::Pull] {
        bounded(Duration::from_secs(60), move || {
            let fx = Fx::new();
            let (src, _) = sides(&fx, dir);
            write(&src.join("plain.txt"), "plain");
            let lease = fx.lease();
            if dir == Direction::Push {
                fx.begin_push(lease);
            }
            let t = fx.transfer(dir);
            let link = Panics(fx.link());
            let ack = Duration::from_secs(30);
            let (_, ran, _) = run(&link, &t, lease, 2, &Abort::default(), ack);
            let why = ran.failure.expect("not finished");
            assert!(why.contains("panicked"), "{dir:?}: {why}");
        });
    }
}
