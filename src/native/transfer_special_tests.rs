//! Sockets, FIFOs and devices in a unit (review B2): never listed on either
//! side, never opened, untouched, and noted. A FIFO's `open(2)` would block
//! forever with no writer, so everything here runs under a bound.

use super::*;
use crate::native::source;
use crate::native::tests_fixture::{Fx, bounded, excludes, manifest, run};
use crate::testutil::write;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixListener;
use std::path::Path;

const LIMIT: Duration = Duration::from_secs(30);

fn fifo(p: &Path) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let c = CString::new(p.as_os_str().as_bytes()).unwrap();
    // SAFETY: `c` is a valid path for the call.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0);
}

fn is_fifo(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_fifo())
}

fn is_socket(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_socket())
}

fn sides(fx: &Fx, dir: Direction) -> (PathBuf, PathBuf) {
    match dir {
        Direction::Push => (fx.leaf(), fx.home_work()),
        Direction::Pull => (fx.home_work(), fx.leaf()),
    }
}

/// `make` puts a special in the source; the transfer finishes, moves the rest,
/// leaves it alone on the source, puts nothing on the target, and notes it.
fn carried_around(make: fn(&Path), there: fn(&Path) -> bool) {
    for dir in [Direction::Push, Direction::Pull] {
        bounded(LIMIT, move || {
            let fx = Fx::new();
            let (src, tgt) = sides(&fx, dir);
            write(&src.join("sub/plain.txt"), "plain\n");
            make(&src.join("sub/s"));
            write(&tgt.join("old.txt"), "the target's");
            let lease = fx.lease();
            if dir == Direction::Push {
                fx.begin_push(lease);
            }
            let t = fx.transfer(dir);
            let (d, ran, _) = run(&fx.link(), &t, lease, 2, &Abort::default(), LIMIT);
            assert!(ran.ok(), "{dir:?}: {ran:?}");
            assert!(
                there(&src.join("sub/s")),
                "{dir:?}: untouched on the source"
            );
            assert!(
                std::fs::symlink_metadata(tgt.join("sub/s")).is_err(),
                "{dir:?}: absent on the target"
            );
            assert_eq!(
                std::fs::read(tgt.join("sub/plain.txt")).unwrap(),
                b"plain\n"
            );
            assert!(!tgt.join("old.txt").exists());
            assert!(d.changes.send.iter().all(|e| e.rel != "sub/s"));
            for notes in [&d.itemized.notes, &ran.itemized.notes] {
                assert!(
                    notes
                        .iter()
                        .any(|n| n.starts_with("1 sockets") && n.contains("sub/s")),
                    "{dir:?}: {notes:?}"
                );
            }
        });
    }
}

#[test]
fn a_socket_in_a_unit_is_carried_around_and_noted() {
    carried_around(
        |p| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::mem::forget(UnixListener::bind(p).unwrap());
        },
        is_socket,
    );
}

#[test]
fn a_fifo_in_a_unit_is_carried_around_and_never_opened() {
    carried_around(fifo, is_fifo);
}

#[test]
fn a_special_on_the_target_is_left_alone_and_its_directory_kept() {
    for dir in [Direction::Push, Direction::Pull] {
        bounded(LIMIT, move || {
            let fx = Fx::new();
            let (src, tgt) = sides(&fx, dir);
            write(&src.join("plain.txt"), "plain\n");
            fifo(&tgt.join("only-target/f"));
            let lease = fx.lease();
            if dir == Direction::Push {
                fx.begin_push(lease);
            }
            let t = fx.transfer(dir);
            let (_, ran, _) = run(&fx.link(), &t, lease, 2, &Abort::default(), LIMIT);
            assert!(ran.ok(), "{dir:?}: {ran:?}");
            assert!(is_fifo(&tgt.join("only-target/f")), "{dir:?}: not deleted");
        });
    }
}

#[test]
fn a_file_whose_name_a_special_holds_on_the_target_fails_and_says_so() {
    for dir in [Direction::Push, Direction::Pull] {
        bounded(LIMIT, move || {
            let fx = Fx::new();
            let (src, tgt) = sides(&fx, dir);
            write(&src.join("clash"), "a file\n");
            fifo(&tgt.join("clash"));
            let lease = fx.lease();
            if dir == Direction::Push {
                fx.begin_push(lease);
            }
            let t = fx.transfer(dir);
            let (_, ran, _) = run(&fx.link(), &t, lease, 1, &Abort::default(), LIMIT);
            let why = ran.failure.clone().expect("not finished");
            assert!(why.contains("socket, FIFO or device"), "{dir:?}: {why}");
            assert!(is_fifo(&tgt.join("clash")), "{dir:?}: not replaced");
        });
    }
}

#[test]
fn a_fifo_swapped_in_after_the_diff_is_never_blocked_on() {
    for dir in [Direction::Push, Direction::Pull] {
        bounded(LIMIT, move || {
            let fx = Fx::new();
            let (src, _) = sides(&fx, dir);
            write(&src.join("was-a-file"), "x");
            write(&src.join("other"), "y");
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
                ack_timeout: LIMIT,
                progress: &progress,
            };
            let d = t.dry_run(&env).unwrap();
            std::fs::remove_file(src.join("was-a-file")).unwrap();
            fifo(&src.join("was-a-file"));
            let ran = t.apply(&env, &d);
            // The source no longer has it: the target's verification agrees
            // once the other file arrived, and nothing blocked getting here.
            assert!(ran.ok(), "{dir:?}: {ran:?}");
            assert!(is_fifo(&src.join("was-a-file")));
        });
    }
}

#[test]
fn opening_a_fifo_neither_blocks_nor_returns_it() {
    bounded(LIMIT, || {
        let t = tempfile::tempdir().unwrap();
        fifo(&t.path().join("f"));
        assert!(source::open(&t.path().join("f")).is_none());
        write(&t.path().join("plain"), "x");
        assert!(source::open(&t.path().join("plain")).is_some());
    });
}

#[test]
fn a_single_file_unit_that_is_a_special_lists_nothing() {
    bounded(LIMIT, || {
        let t = tempfile::tempdir().unwrap();
        fifo(&t.path().join("f"));
        let m = Manifest::walk(&t.path().join("f"), &excludes(), true).unwrap();
        assert!(m.entries.is_empty());
        assert_eq!(m.skipped, [""]);
    });
}

#[test]
fn every_walk_agrees_a_special_is_not_there() {
    let t = tempfile::tempdir().unwrap();
    let r = t.path().join("u");
    write(&r.join("plain"), "x");
    let before_m = manifest(&r);
    let ex = crate::exclude::Excludes::default();
    let before_d = crate::repos::discover(&r, &ex).unwrap();
    let before_s = crate::state::loose(&before_d.loose);
    fifo(&r.join("f"));
    assert_eq!(manifest(&r).entries, before_m.entries, "the manifest");
    assert_eq!(manifest(&r).skipped, ["f"]);
    let d = crate::repos::discover(&r, &ex).unwrap();
    assert_eq!(d, before_d, "discovery");
    assert_eq!(crate::state::loose(&d.loose), before_s, "the state hash");
    assert!(
        !crate::repos::all_names(&r, &ex)
            .unwrap()
            .contains(&"f".to_string()),
        "the collision check"
    );
    // As a base, it exists and holds nothing.
    let base = crate::repos::discover(&r.join("f"), &ex).unwrap();
    assert!(base.exists && base.loose.is_empty(), "{base:?}");
    // An excluded special is no one's business: not even a note.
    let ex2 = crate::exclude::Excludes::for_root(&["f".to_string()]).unwrap();
    assert!(Manifest::walk(&r, &ex2, false).unwrap().skipped.is_empty());
}

#[test]
fn a_special_has_no_entry_and_no_copy() {
    bounded(LIMIT, || {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("f");
        fifo(&f);
        let m = std::fs::symlink_metadata(&f).unwrap();
        let e = crate::native::Entry::of(&f, "f", &m)
            .unwrap_err()
            .to_string();
        assert!(e.contains("socket, FIFO or device"), "{e}");
        assert!(crate::native::backup::copy_tree(&f, &t.path().join("to")).is_err());
        assert!(!t.path().join("to").exists());
    });
}
