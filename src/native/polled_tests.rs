//! The polled socket: data flows, a timeout ends a read by the clock, and a
//! stop from another thread ends it at once, every time, through the pipe,
//! whatever the socket's own wakeup does.
//!
//! Every read whose only way out is what is under test runs under
//! `bounded`, so a regression fails by name rather than hanging the suite
//! (review A45).

use super::*;
use crate::native::tests_fixture::bounded;
use std::time::Instant;

const LIMIT: Duration = Duration::from_secs(20);

fn pair() -> (Polled<UnixStream>, Stop, UnixStream) {
    let (a, b) = UnixStream::pair().unwrap();
    let (p, stop) = Polled::unix(a).unwrap();
    (p, stop, b)
}

#[test]
fn data_flows_both_ways_and_a_closed_peer_reads_as_eof() {
    bounded(LIMIT, || {
        let (mut p, _stop, mut peer) = pair();
        peer.write_all(b"hello").unwrap();
        let mut buf = [0u8; 8];
        assert_eq!(p.read(&mut buf).unwrap(), 5);
        assert_eq!(&buf[..5], b"hello");
        p.write_all(b"back").unwrap();
        let mut got = [0u8; 4];
        peer.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"back");
        drop(peer);
        assert_eq!(p.read(&mut buf).unwrap(), 0, "EOF");
    });
}

#[test]
fn a_timeout_ends_a_read_by_the_clock_and_none_waits() {
    bounded(LIMIT, || {
        let (mut p, stop, _peer) = pair();
        stop.set_timeout(Duration::from_millis(200)).unwrap();
        let t = Instant::now();
        let e = p.read(&mut [0u8; 8]).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut, "{e}");
        let took = t.elapsed();
        assert!(
            took >= Duration::from_millis(200) && took < Duration::from_secs(2),
            "{took:?}"
        );
    });
    // Under a millisecond still polls rather than waiting forever.
    assert_eq!(to_ms(Duration::from_micros(10)), 1);
    assert_eq!(to_ms(Duration::from_millis(1500)), 1500);
}

/// The stop ends a read through the pipe, so it does not depend on the
/// socket's shutdown waking anyone: 2,000 rounds of a read and a stop from
/// another thread, racing. Whichever lands first, the read ends at once;
/// the rounds where the read was already blocked are the ones that matter,
/// and nothing picks them out (review A49).
#[test]
fn a_stop_from_another_thread_ends_a_read_at_once_every_time() {
    bounded(Duration::from_secs(120), || {
        let mut worst = Duration::ZERO;
        for _ in 0..2_000 {
            let (mut p, stop, _peer) = pair();
            let began = Instant::now();
            std::thread::scope(|s| {
                s.spawn(move || stop.stop());
                let e = p.read(&mut [0u8; 8]).unwrap_err();
                assert!(
                    matches!(
                        e.kind(),
                        io::ErrorKind::ConnectionAborted | io::ErrorKind::UnexpectedEof
                    ) || e.to_string().contains("stopped"),
                    "{e}"
                );
            });
            worst = worst.max(began.elapsed());
        }
        assert!(worst < Duration::from_secs(2), "worst round {worst:?}");
    });
}

/// A read that is blocked for certain when the stop lands: the stop waits
/// until the reader has been in its read for a while.
#[test]
fn a_stop_ends_a_read_that_was_already_blocked() {
    bounded(LIMIT, || {
        let (mut p, stop, _peer) = pair();
        let t = Instant::now();
        std::thread::scope(|s| {
            s.spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                stop.stop();
            });
            let e = p.read(&mut [0u8; 8]).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::ConnectionAborted, "{e}");
        });
        let took = t.elapsed();
        assert!(
            took >= Duration::from_millis(300) && took < Duration::from_secs(2),
            "{took:?}"
        );
    });
}

#[test]
fn a_stop_raised_many_times_never_blocks_and_a_clone_stops_too() {
    bounded(LIMIT, || {
        let (mut p, stop, _peer) = pair();
        let clone = stop.try_clone().unwrap();
        // Far more than a pipe holds: the write end is non-blocking.
        for _ in 0..100_000 {
            stop.stop();
        }
        assert!(p.read(&mut [0u8; 8]).is_err());
        let (mut p2, stop2, _peer2) = pair();
        let c2 = stop2.try_clone().unwrap();
        drop(stop2);
        c2.stop();
        assert!(p2.read(&mut [0u8; 8]).is_err());
        drop(clone);
    });
}

/// What the doc on `stop` promises beyond the read: a write blocked on a
/// full buffer fails, and the peer reads EOF (review A46; the shutdown
/// half of `stop`).
#[test]
fn a_stop_fails_a_blocked_write_and_the_peer_reads_eof() {
    bounded(LIMIT, || {
        let (mut p, stop, mut peer) = pair();
        stop.set_timeout(Duration::from_secs(10)).unwrap();
        let t = Instant::now();
        std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(Duration::from_millis(300));
                stop.stop();
            });
            // Fill the socket until the write blocks; nobody drains it.
            let chunk = [0u8; 64 * 1024];
            let e = loop {
                if let Err(e) = p.write_all(&chunk) {
                    break e;
                }
            };
            assert_ne!(
                e.kind(),
                io::ErrorKind::WouldBlock,
                "ended by the stop, not the clock: {e}"
            );
        });
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
        // The peer: what was written, then EOF.
        let mut sink = Vec::new();
        peer.read_to_end(&mut sink).unwrap();
        assert!(!sink.is_empty());
    });
}

#[test]
fn tcp_is_wrapped_the_same_way_and_keeps_its_timeouts() {
    bounded(LIMIT, || {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let s = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (_peer, _) = l.accept().unwrap();
        let (mut p, stop) = Polled::tcp(s).unwrap();
        p.set_read_timeout(Some(Duration::from_millis(150)))
            .unwrap();
        p.set_write_timeout(Some(Duration::from_secs(7))).unwrap();
        assert_eq!(p.read_timeout().unwrap(), Some(Duration::from_millis(150)));
        assert_eq!(p.write_timeout().unwrap(), Some(Duration::from_secs(7)));
        let t = Instant::now();
        assert_eq!(
            p.read(&mut [0u8; 8]).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(t.elapsed() < Duration::from_secs(2));
        assert_eq!(p.inner().peer_addr().unwrap(), l.local_addr().unwrap());
        let second = p.stop().unwrap();
        second.stop();
        assert!(p.read(&mut [0u8; 8]).is_err());
        drop(stop);
    });
}

/// A stop raised after the connection it belonged to is gone (the monitor
/// shuts everything once the sends are over; by then a worker may have
/// dropped its `Conn`) must not write to a pipe nobody reads: that is EPIPE,
/// and SIGPIPE to a process with the default disposition (review A41). The
/// handle keeps the read end alive, so the write lands, never EPIPE. Checked
/// on the pipe directly, with no signal involved.
#[test]
fn a_stop_after_the_connection_is_dropped_never_hits_a_closed_pipe() {
    let (p, stop, _peer) = pair();
    let clone = stop.try_clone().unwrap();
    drop(p);
    for s in [&stop, &clone] {
        let r = (&s.wake).write(&[1]);
        assert!(r.is_ok(), "{r:?}");
    }
    drop(stop);
    let r = (&clone.wake).write(&[1]);
    assert!(r.is_ok(), "{r:?}");
    clone.stop();
    // And what the OS gives without the shared read end, so the assertion
    // above is known to be able to fail.
    let (woken, wake) = io::pipe().unwrap();
    drop(woken);
    assert_eq!(
        (&wake).write(&[1]).unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
}
