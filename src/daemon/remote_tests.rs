use super::*;
use crate::daemon::certs;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct Ping {
    n: u32,
    text: String,
}

fn bundle(dir: &Path) -> PathBuf {
    let b = dir.join("credentials.pem");
    certs::ensure(&dir.join("ca.key"), &dir.join("ca.pem"), &b).unwrap();
    b
}

/// An echo server on loopback that answers one connection.
fn echo_once(listener: Arc<MtlsListener>) -> std::thread::JoinHandle<io::Result<()>> {
    std::thread::spawn(move || {
        let tcp = listener.accept()?;
        let mut s = listener.secure(tcp)?;
        let mut p: Ping = recv(&mut s).map_err(io::Error::other)?;
        p.n += 1;
        send(&mut s, &p).map_err(io::Error::other)?;
        Ok(())
    })
}

#[test]
fn round_trip_over_loopback_with_mtls() {
    let tmp = tempfile::tempdir().unwrap();
    let id = Identity::load(&bundle(tmp.path())).unwrap();
    let l = Arc::new(MtlsListener::bind("127.0.0.1:0", &id).unwrap());
    let addr = l.local_addr().to_string();
    let server = echo_once(l);
    let d = Dialer::new(&id, Duration::from_secs(5)).unwrap();
    let got: Ping = d
        .call(
            &addr,
            &Ping {
                n: 41,
                text: "héllo\nworld".into(),
            },
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(
        got,
        Ping {
            n: 42,
            text: "héllo\nworld".into()
        }
    );
    server.join().unwrap().unwrap();
}

#[test]
fn a_client_with_another_ca_is_refused_in_the_handshake() {
    let home = tempfile::tempdir().unwrap();
    let stranger = tempfile::tempdir().unwrap();
    let home_id = Identity::load(&bundle(home.path())).unwrap();
    let other_id = Identity::load(&bundle(stranger.path())).unwrap();
    let l = Arc::new(MtlsListener::bind("127.0.0.1:0", &home_id).unwrap());
    let addr = l.local_addr().to_string();
    let server = echo_once(l);
    let d = Dialer::new(&other_id, Duration::from_secs(5)).unwrap();
    let err = d
        .call::<_, Ping>(
            &addr,
            &Ping {
                n: 1,
                text: String::new(),
            },
            Duration::from_secs(5),
        )
        .unwrap_err();
    assert!(matches!(err, Error::Tls(_)), "{err}");
    assert!(
        server.join().unwrap().is_err(),
        "the server never reached a handler"
    );
}

#[test]
fn a_plaintext_client_never_reaches_a_handler() {
    let tmp = tempfile::tempdir().unwrap();
    let id = Identity::load(&bundle(tmp.path())).unwrap();
    let l = Arc::new(MtlsListener::bind("127.0.0.1:0", &id).unwrap());
    let addr = l.local_addr();
    let server = echo_once(l);
    let mut raw = TcpStream::connect(addr).unwrap();
    raw.write_all(b"{\"n\":1,\"text\":\"\"}\n").unwrap();
    drop(raw);
    assert!(server.join().unwrap().is_err());
}

#[test]
fn unspecified_bind_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let id = Identity::load(&bundle(tmp.path())).unwrap();
    let err = MtlsListener::bind("0.0.0.0:0", &id)
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("every interface"), "{err}");
}

#[test]
fn unreachable_home_is_a_remote_error() {
    let tmp = tempfile::tempdir().unwrap();
    let id = Identity::load(&bundle(tmp.path())).unwrap();
    // Bind and drop to find a port nobody listens on.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let d = Dialer::new(&id, Duration::from_millis(500)).unwrap();
    let err = d.call::<_, Ping>(
        &format!("127.0.0.1:{port}"),
        &Ping {
            n: 0,
            text: String::new(),
        },
        Duration::from_secs(1),
    );
    assert!(matches!(err, Err(Error::Remote(_))), "{err:?}");
}

#[test]
fn recv_rejects_empty_truncated_and_garbage() {
    assert!(
        recv::<_, Ping>(&b""[..])
            .unwrap_err()
            .to_string()
            .contains("closed")
    );
    assert!(
        recv::<_, Ping>(&b"{\"n\":1"[..])
            .unwrap_err()
            .to_string()
            .contains("truncated")
    );
    assert!(
        recv::<_, Ping>(&b"nope\n"[..])
            .unwrap_err()
            .to_string()
            .contains("unreadable")
    );
    let mut buf = Vec::new();
    send(
        &mut buf,
        &Ping {
            n: 3,
            text: "x".into(),
        },
    )
    .unwrap();
    assert_eq!(recv::<_, Ping>(&buf[..]).unwrap().n, 3);
}

/// F8: the probe home's queue wait uses, on real sockets.
#[test]
fn connected_sees_a_peer_leave_and_leaves_the_socket_blocking() {
    use std::os::fd::AsRawFd;
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(l.local_addr().unwrap()).unwrap();
    let (server, _) = l.accept().unwrap();
    assert!(connected(&server), "an idle peer is connected");
    let flags = unsafe { libc::fcntl(server.as_raw_fd(), libc::F_GETFL) };
    assert_eq!(
        flags & libc::O_NONBLOCK,
        0,
        "the probe restores blocking mode"
    );

    client.write_all(b"x").unwrap();
    let mut b = [0u8; 1];
    server.peek(&mut b).unwrap();
    assert!(connected(&server), "bytes waiting: still connected");
    assert_eq!(
        (&server).read(&mut b).unwrap(),
        1,
        "the probe consumed nothing"
    );

    drop(client);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while connected(&server) {
        assert!(
            std::time::Instant::now() < deadline,
            "a closed peer still reads as connected"
        );
        std::thread::yield_now();
    }
    assert!(!connected(&server), "and stays gone");
}
