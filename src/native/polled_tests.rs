//! The polled socket: data flows, a timeout ends a read by the clock, and a
//! stop from another thread ends it at once, every time, through the pipe,
//! whatever the socket's own wakeup does.

use super::*;
use std::time::Instant;

fn pair() -> (Polled<UnixStream>, Stop, UnixStream) {
    let (a, b) = UnixStream::pair().unwrap();
    let (p, stop) = Polled::unix(a).unwrap();
    (p, stop, b)
}

#[test]
fn data_flows_both_ways_and_a_closed_peer_reads_as_eof() {
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
}

#[test]
fn a_timeout_ends_a_read_by_the_clock_and_none_waits() {
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
    // Under a millisecond still polls rather than waiting forever.
    assert_eq!(to_ms(Duration::from_micros(10)), 1);
    assert_eq!(to_ms(Duration::from_millis(1500)), 1500);
}

/// The stop ends a blocked read through the pipe, so it does not depend on
/// the socket's shutdown waking anyone: 2,000 rounds, each a read that is
/// already blocked when the stop lands, from another thread.
#[test]
fn a_stop_from_another_thread_ends_a_blocked_read_at_once_every_time() {
    let mut worst = Duration::ZERO;
    for _ in 0..2_000 {
        let (mut p, stop, _peer) = pair();
        let began = Instant::now();
        std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::spawn(move || stop.stop());
            });
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
}

#[test]
fn a_stop_raised_many_times_never_blocks_and_a_clone_stops_too() {
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
}

#[test]
fn tcp_is_wrapped_the_same_way_and_keeps_its_timeouts() {
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
}
