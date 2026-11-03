//! A transfer that does not finish: a stalled receiver, a cut connection,
//! an abort, home dropping the lease. Each ends the unit with why, leaves
//! the target holding only whole files, and the next run completes it.

use super::*;
use crate::native::tests_fixture::{Fault, Faulty, Fx, manifest, run, tree};
use crate::testutil::write;
use std::time::Instant;

/// Every file the target holds is whole: it is the source's (same size and
/// mtime) or the target's own from before; no temporary is left.
fn only_whole_files(fx: &Fx, dir: Direction, before: &crate::native::Manifest) {
    let (src, tgt) = match dir {
        Direction::Push => (fx.leaf(), fx.home_work()),
        Direction::Pull => (fx.home_work(), fx.leaf()),
    };
    let (src, tgt) = (manifest(&src), manifest(&tgt));
    assert!(tgt.temps.is_empty(), "{:?}", tgt.temps);
    for e in tgt.entries.iter().filter(|e| !e.is_dir()) {
        let ok = src.get(&e.rel) == Some(e) || before.get(&e.rel) == Some(e);
        assert!(ok, "{} is neither the source's nor the old copy", e.rel);
    }
}

fn resumes(fx: &Fx, t: &Transfer, lease: u64) {
    let (_, ran, _) = run(
        &fx.link(),
        t,
        lease,
        4,
        &Abort::default(),
        Duration::from_secs(20),
    );
    assert!(ran.ok(), "the next run finishes: {ran:?}");
    let (s, d) = match t.direction {
        Direction::Push => (fx.leaf(), fx.home_work()),
        Direction::Pull => (fx.home_work(), fx.leaf()),
    };
    assert_eq!(manifest(&d), manifest(&s));
}

#[test]
fn a_receiver_that_never_acks_is_a_dead_connection_at_the_ack_timeout() {
    for dir in [Direction::Push, Direction::Pull] {
        let fx = Fx::new();
        let (src, tgt) = match dir {
            Direction::Push => (fx.leaf(), fx.home_work()),
            Direction::Pull => (fx.home_work(), fx.leaf()),
        };
        tree(&src, 4);
        write(&tgt.join("big/blob0.bin"), "the target's own copy");
        let before = manifest(&tgt);
        let lease = fx.lease();
        if dir == Direction::Push {
            fx.begin_push(lease);
        }
        let t = fx.transfer(dir);
        let link = Faulty::new(fx.link(), Fault::Stall(1));
        let started = Instant::now();
        let (_, ran, _) = run(
            &link,
            &t,
            lease,
            4,
            &Abort::default(),
            Duration::from_millis(400),
        );
        let why = ran.failure.clone().expect("not finished");
        assert!(why.contains("connection is dead"), "{dir:?}: {why}");
        assert!(!ran.stopped);
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "{:?}",
            started.elapsed()
        );
        only_whole_files(&fx, dir, &before);
        resumes(&fx, &t, lease);
    }
}

#[test]
fn a_connection_cut_mid_unit_interrupts_it_and_the_next_run_completes() {
    for (dir, limit) in [(Direction::Push, 300_000), (Direction::Pull, 300_000)] {
        let fx = Fx::new();
        let (src, tgt) = match dir {
            Direction::Push => (fx.leaf(), fx.home_work()),
            Direction::Pull => (fx.home_work(), fx.leaf()),
        };
        tree(&src, 3);
        let before = manifest(&tgt);
        let lease = fx.lease();
        if dir == Direction::Push {
            fx.begin_push(lease);
        }
        let t = fx.transfer(dir);
        let link = Faulty::new(fx.link(), Fault::CutAfter(0, limit));
        let t0 = Instant::now();
        let (_, ran, _) = run(
            &link,
            &t,
            lease,
            2,
            &Abort::default(),
            Duration::from_secs(20),
        );
        assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
        assert!(!ran.ok(), "{dir:?}: the cut is noticed");
        only_whole_files(&fx, dir, &before);
        resumes(&fx, &t, lease);
    }
}

#[test]
fn an_abort_stops_every_worker_promptly() {
    let fx = Fx::new();
    tree(&fx.leaf(), 4);
    let before = manifest(&fx.home_work());
    let lease = fx.lease();
    fx.begin_push(lease);
    let t = fx.transfer(Direction::Push);
    // One connection stalls with a long ack timeout: only the abort ends it.
    let link = Faulty::new(fx.link(), Fault::Stall(0));
    let abort = Abort::default();
    let started = Instant::now();
    let ran = std::thread::scope(|s| {
        s.spawn(|| {
            std::thread::sleep(Duration::from_millis(300));
            abort.raise_because("the CLI hung up (Ctrl-C, or it was killed)");
        });
        run(&link, &t, lease, 4, &abort, Duration::from_secs(60)).1
    });
    assert!(ran.stopped, "{ran:?}");
    assert!(ran.failure.unwrap().contains("the CLI hung up"));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    only_whole_files(&fx, Direction::Push, &before);
}

#[test]
fn home_dropping_the_lease_mid_unit_stops_home_writing() {
    let fx = Fx::new();
    tree(&fx.leaf(), 3);
    let before = manifest(&fx.home_work());
    let lease = fx.lease();
    fx.begin_push(lease);
    let t = fx.transfer(Direction::Push);
    let link = Faulty::new(fx.link(), Fault::Throttle(0, Duration::from_millis(2)));
    let abort = Abort::default();
    let dropped = std::sync::atomic::AtomicBool::new(false);
    let progress = |_: Counts| {
        if !dropped.swap(true, std::sync::atomic::Ordering::SeqCst) {
            fx.release(lease);
        }
    };
    let env = Env {
        link: &link,
        me: "leaf",
        lease,
        abort: &abort,
        streams: 1,
        ack_timeout: Duration::from_secs(20),
        progress: &progress,
    };
    let d = t.dry_run(&env).unwrap();
    let ran = t.apply(&env, &d);
    assert!(
        dropped.load(std::sync::atomic::Ordering::SeqCst),
        "released mid-unit"
    );
    let why = ran.failure.expect("home stopped");
    assert!(
        why.contains("head of home's queue") || why.contains("not queued"),
        "{why}"
    );
    only_whole_files(&fx, Direction::Push, &before);
}

#[test]
fn a_dead_runs_temporaries_are_removed_by_the_next_transfer() {
    for dir in [Direction::Push, Direction::Pull] {
        let fx = Fx::new();
        let (src, tgt) = match dir {
            Direction::Push => (fx.leaf(), fx.home_work()),
            Direction::Pull => (fx.home_work(), fx.leaf()),
        };
        tree(&src, 1);
        write(
            &tgt.join("small/000/.replicant-tmp-f0.txt"),
            "half of a dead run",
        );
        write(&tgt.join(".replicant-tmp-x"), "half");
        let lease = fx.lease();
        if dir == Direction::Push {
            fx.begin_push(lease);
        }
        let t = fx.transfer(dir);
        let (d, ran, _) = run(
            &fx.link(),
            &t,
            lease,
            2,
            &Abort::default(),
            Duration::from_secs(20),
        );
        assert_eq!(d.temps.len(), 2, "{dir:?}");
        assert!(ran.ok(), "{ran:?}");
        assert!(manifest(&tgt).temps.is_empty());
        assert!(!tgt.join(".replicant-tmp-x").exists());
    }
}
