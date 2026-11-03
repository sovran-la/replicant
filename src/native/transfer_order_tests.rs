//! The order of a transfer, by what a cut in the middle of it leaves behind
//! (review M4): deletions only once every file arrived, refs and the index
//! only once everything else did. Each was checked against the mutant it
//! exists for (deletions moved ahead of the sends, the rounds swapped).

use super::*;
use crate::native::conn::{Conn, Sock};
use crate::native::frame::{Frame, Kind};
use crate::native::tests_fixture::{Fault, Faulty, Fx, bounded, run, tree};
use crate::testutil::write;

/// Cut one connection a little way into the sends: Clean, Replace, Mkdirs and
/// a mutant's early Deletes get through, three 2 MiB files do not.
const CUT: usize = 300_000;

fn cut(fx: &Fx, dir: Direction) {
    let lease = fx.lease();
    if dir == Direction::Push {
        fx.begin_push(lease);
    }
    let t = fx.transfer(dir);
    let link = Faulty::new(fx.link(), Fault::CutAfter(0, CUT));
    let ack = Duration::from_secs(20);
    let (_, ran, _) = run(&link, &t, lease, 1, &Abort::default(), ack);
    assert!(!ran.ok(), "{dir:?}: the cut is noticed");
}

#[test]
fn a_push_deletes_only_after_every_file_arrived() {
    let fx = Fx::new();
    tree(&fx.leaf(), 3);
    write(&fx.home_work().join("only-home.txt"), "still needed");
    cut(&fx, Direction::Push);
    assert!(fx.home_work().join("only-home.txt").exists());
}

#[test]
fn a_pull_deletes_only_after_every_file_arrived() {
    let fx = Fx::new();
    tree(&fx.home_work(), 3);
    write(&fx.leaf().join("only-leaf.txt"), "still needed");
    cut(&fx, Direction::Pull);
    assert!(fx.leaf().join("only-leaf.txt").exists());
}

/// `base/r` holds a repo's shape: large objects, and what points at them.
fn repo_shape(base: &std::path::Path) {
    let git = base.join("r/.git");
    for i in 0..3 {
        let p = git.join(format!("objects/pack/{i}.pack"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![i as u8; 2 << 20]).unwrap();
    }
    write(&git.join("HEAD"), "ref: refs/heads/main\n");
    write(&git.join("packed-refs"), "x\n");
    write(&git.join("refs/heads/main"), "0000\n");
    write(&git.join("index"), "index\n");
}

fn no_refs_arrived(base: &std::path::Path, dir: Direction) {
    for f in ["HEAD", "packed-refs", "refs/heads/main", "index"] {
        assert!(!base.join("r/.git").join(f).exists(), "{dir:?}: {f}");
    }
}

#[test]
fn a_push_sends_refs_only_after_everything_else_arrived() {
    let fx = Fx::new();
    repo_shape(&fx.leaf());
    cut(&fx, Direction::Push);
    no_refs_arrived(&fx.home_work(), Direction::Push);
}

#[test]
fn a_pull_takes_refs_only_after_everything_else_arrived() {
    let fx = Fx::new();
    repo_shape(&fx.home_work());
    cut(&fx, Direction::Pull);
    no_refs_arrived(&fx.leaf(), Direction::Pull);
}

/// p18: a peer that stops draining, with nothing shut down: the write itself
/// fails at the timeout. Plain TCP through the production `Conn`, as the
/// socket under TLS is. Checked against the mutant with no write timeout.
#[test]
fn a_write_to_a_silent_peer_fails_at_the_timeout() {
    bounded(Duration::from_secs(30), || {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let s = std::net::TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (_silent, _) = l.accept().unwrap();
        let sock = Sock::Tcp(s.try_clone().unwrap());
        let mut c = Conn::new(Box::new(s), sock);
        c.set_timeout(Duration::from_millis(300)).unwrap();
        let frame = Frame::new(Kind::Chunk, vec![0; 1 << 20]);
        let began = std::time::Instant::now();
        let e = loop {
            if let Err(e) = c.send(&frame) {
                break e.to_string();
            }
            assert!(began.elapsed() < Duration::from_secs(20), "never blocked");
        };
        assert!(e.contains("connection is dead"), "{e}");
    });
}
