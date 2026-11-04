use super::*;
use crate::daemon::certs;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct Ping {
    n: u32,
    text: String,
}

/// Home's server identity and a client identity (the bootstrap) from one CA.
fn ids(dir: &Path) -> (Identity, Identity) {
    let p = certs::Paths::of(&crate::config::Dirs::at(dir.to_path_buf()));
    certs::ensure(&p).unwrap();
    (
        Identity::load(&p.server).unwrap(),
        Identity::load(&p.bootstrap).unwrap(),
    )
}

fn ping(d: &Dialer, addr: &str) -> Result<Ping> {
    d.call(
        addr,
        &Ping {
            n: 1,
            text: String::new(),
        },
        Duration::from_secs(5),
    )
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
    let (server_id, client_id) = ids(tmp.path());
    let l = Arc::new(MtlsListener::bind("127.0.0.1:0", &server_id).unwrap());
    let addr = l.local_addr().to_string();
    let server = echo_once(l);
    let d = Dialer::new(&client_id, Duration::from_secs(5)).unwrap();
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
    let home_id = ids(home.path()).0;
    let other_id = ids(stranger.path()).1;
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

/// Home's server certificate is server-only and every client certificate
/// client-only: neither end can stand in for the other.
#[test]
fn a_client_identity_cannot_serve_and_the_server_identity_cannot_dial() {
    let tmp = tempfile::tempdir().unwrap();
    let (server_id, client_id) = ids(tmp.path());

    let l = Arc::new(MtlsListener::bind("127.0.0.1:0", &client_id).unwrap());
    let addr = l.local_addr().to_string();
    let server = echo_once(l);
    let err = ping(
        &Dialer::new(&client_id, Duration::from_secs(5)).unwrap(),
        &addr,
    );
    assert!(matches!(err, Err(Error::Tls(_))), "{err:?}");
    assert!(server.join().unwrap().is_err());

    let l = Arc::new(MtlsListener::bind("127.0.0.1:0", &server_id).unwrap());
    let addr = l.local_addr().to_string();
    let server = echo_once(l);
    let _ = ping(
        &Dialer::new(&server_id, Duration::from_secs(5)).unwrap(),
        &addr,
    );
    assert!(
        server.join().unwrap().is_err(),
        "home refused its own server certificate as a client"
    );
}

/// The pre-v0.2 shared certificate is good for both uses and every early
/// leaf holds its key. A "home" presenting it is refused by name.
#[test]
fn a_home_presenting_the_old_shared_certificate_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let p = certs::Paths::of(&crate::config::Dirs::at(tmp.path().to_path_buf()));
    certs::ensure(&p).unwrap();
    let legacy = crate::daemon::tls::parse(&certs::legacy_bundle(&p).unwrap(), &p.server).unwrap();
    let client_id = Identity::load(&p.bootstrap).unwrap();
    let l = Arc::new(MtlsListener::bind("127.0.0.1:0", &legacy).unwrap());
    let addr = l.local_addr().to_string();
    let server = echo_once(l);
    let err = ping(
        &Dialer::new(&client_id, Duration::from_secs(5)).unwrap(),
        &addr,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("pre-v0.2 shared bundle"), "{err}");
    let _ = server.join();

    // The same certificate is still a usable *client* certificate: after the
    // upgrade it is home's bootstrap.
    let server_id = Identity::load(&p.server).unwrap();
    let l = Arc::new(MtlsListener::bind("127.0.0.1:0", &server_id).unwrap());
    let addr = l.local_addr().to_string();
    let server = echo_once(l);
    assert_eq!(
        ping(
            &Dialer::new(&legacy, Duration::from_secs(5)).unwrap(),
            &addr
        )
        .unwrap()
        .n,
        2
    );
    server.join().unwrap().unwrap();
}

#[test]
fn a_plaintext_client_never_reaches_a_handler() {
    let tmp = tempfile::tempdir().unwrap();
    let id = ids(tmp.path()).0;
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
    let id = ids(tmp.path()).0;
    let err = MtlsListener::bind("0.0.0.0:0", &id)
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("every interface"), "{err}");
}

#[test]
fn unreachable_home_is_a_remote_error() {
    let tmp = tempfile::tempdir().unwrap();
    let id = ids(tmp.path()).1;
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

/// A loopback port nobody listens on.
fn dead_port() -> String {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    format!("127.0.0.1:{port}")
}

fn p(n: u32) -> Ping {
    Ping {
        n,
        text: String::new(),
    }
}

#[test]
fn several_addresses_fall_through_to_the_one_that_answers_and_stick() {
    let tmp = tempfile::tempdir().unwrap();
    let (server_id, client_id) = ids(tmp.path());
    let l = Arc::new(MtlsListener::bind("127.0.0.1:0", &server_id).unwrap());
    let live = l.local_addr().to_string();
    let dead = dead_port();
    let d = Dialer::new(&client_id, Duration::from_millis(500)).unwrap();
    assert_eq!(d.preferred(), None);

    let server = echo_once(l.clone());
    let addresses = vec![dead.clone(), live.clone()];
    let got: Ping = d
        .call_any(&addresses, &p(1), Duration::from_secs(5))
        .unwrap();
    assert_eq!(got.n, 2);
    server.join().unwrap().unwrap();
    let chosen = d.preferred().unwrap();
    assert_eq!(chosen.entry, live);
    assert_eq!(chosen.addr, l.local_addr());

    // Preferred first from now on, in a clone too; the dead one last.
    let (order, pref) = d.clone().order(&addresses);
    assert_eq!(order, vec![&live, &dead]);
    assert_eq!(pref.as_ref(), Some(&chosen));
    // A preferred address that is no longer listed is ignored.
    let other = vec![dead.clone()];
    assert_eq!(d.order(&other), (vec![&dead], None));

    // Every address dead: one line per attempt, and the preference is
    // dropped so the next dial starts from the top of the list.
    drop(l);
    let err = d
        .call_any::<_, Ping>(&addresses, &p(0), Duration::from_secs(1))
        .unwrap_err();
    assert!(matches!(err, Error::Remote(_)), "{err}");
    let text = err.to_string();
    assert!(text.contains("did not answer at any address:\n"), "{text}");
    assert!(
        text.contains(&format!("\n  {dead}: ")) && text.contains(&format!("\n  {live}: ")),
        "{text}"
    );
    assert_eq!(text.matches("Connection refused").count(), 2, "{text}");
    assert_eq!(d.preferred(), None);

    // One address keeps the single-address error shape.
    let err = d
        .call_any::<_, Ping>(&other, &p(0), Duration::from_secs(1))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains(&format!("home at {dead} is unreachable: ")) && !err.contains("any address"),
        "{err}"
    );
    assert!(matches!(
        d.connect_any(&[], Duration::from_secs(1)).map(|_| ()),
        Err(Error::Remote(m)) if m.contains("no address")
    ));
}

#[test]
fn a_dead_preferred_address_is_abandoned_for_one_that_answers() {
    let tmp = tempfile::tempdir().unwrap();
    let (server_id, client_id) = ids(tmp.path());
    let a = Arc::new(MtlsListener::bind("127.0.0.1:0", &server_id).unwrap());
    let b = Arc::new(MtlsListener::bind("127.0.0.1:0", &server_id).unwrap());
    let (addr_a, addr_b) = (a.local_addr().to_string(), b.local_addr().to_string());
    let addresses = vec![addr_a.clone(), addr_b.clone()];
    let d = Dialer::new(&client_id, Duration::from_millis(500)).unwrap();

    let server = echo_once(a.clone());
    let _: Ping = d
        .call_any(&addresses, &p(1), Duration::from_secs(5))
        .unwrap();
    server.join().unwrap().unwrap();
    assert_eq!(d.preferred().unwrap().entry, addr_a);

    // A dies; the next dial moves to B and stays there.
    drop(a);
    let server = echo_once(b.clone());
    let _: Ping = d
        .call_any(&addresses, &p(1), Duration::from_secs(5))
        .unwrap();
    server.join().unwrap().unwrap();
    assert_eq!(d.preferred().unwrap().entry, addr_b);
    assert_eq!(d.order(&addresses).0, vec![&addr_b, &addr_a]);
}

#[test]
fn a_refused_handshake_at_one_address_does_not_block_another() {
    let (home, stranger) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (home_id, client_id) = ids(home.path());
    let stranger_id = ids(stranger.path()).0;
    let wrong = Arc::new(MtlsListener::bind("127.0.0.1:0", &stranger_id).unwrap());
    let right = Arc::new(MtlsListener::bind("127.0.0.1:0", &home_id).unwrap());
    let addresses = vec![
        wrong.local_addr().to_string(),
        right.local_addr().to_string(),
    ];
    let d = Dialer::new(&client_id, Duration::from_secs(5)).unwrap();

    let refuser = echo_once(wrong.clone());
    let server = echo_once(right.clone());
    let got: Ping = d
        .call_any(&addresses, &p(1), Duration::from_secs(5))
        .unwrap();
    assert_eq!(got.n, 2);
    assert!(
        refuser.join().unwrap().is_err(),
        "the stranger never got a handler"
    );
    server.join().unwrap().unwrap();
    assert_eq!(d.preferred().unwrap().entry, addresses[1]);

    // Every address a stranger: the error says so, as a TLS error.
    let wrong2 = Arc::new(MtlsListener::bind("127.0.0.1:0", &stranger_id).unwrap());
    let strangers = vec![
        wrong.local_addr().to_string(),
        wrong2.local_addr().to_string(),
    ];
    let (r1, r2) = (echo_once(wrong.clone()), echo_once(wrong2.clone()));
    let err = d
        .call_any::<_, Ping>(&strangers, &p(0), Duration::from_secs(5))
        .unwrap_err();
    assert!(matches!(err, Error::Tls(_)), "{err}");
    let text = err.to_string();
    assert!(text.contains("none accepted the handshake"), "{text}");
    assert_eq!(text.matches("\n  127.0.0.1:").count(), 2, "{text}");
    assert!(r1.join().unwrap().is_err() && r2.join().unwrap().is_err());
}

/// Something that accepts TCP and then says nothing: the walk moves on after
/// the handshake bound, not after the request deadline.
#[test]
fn a_silent_address_costs_the_handshake_bound_not_the_deadline() {
    let tmp = tempfile::tempdir().unwrap();
    let (server_id, client_id) = ids(tmp.path());
    let silent = TcpListener::bind("127.0.0.1:0").unwrap();
    let live = Arc::new(MtlsListener::bind("127.0.0.1:0", &server_id).unwrap());
    let addresses = vec![
        silent.local_addr().unwrap().to_string(),
        live.local_addr().to_string(),
    ];
    let d = Dialer::new(&client_id, Duration::from_secs(5))
        .unwrap()
        .with_handshake_timeout(Duration::from_millis(300));
    let server = echo_once(live.clone());
    let started = std::time::Instant::now();
    let got: Ping = d
        .call_any(&addresses, &p(1), Duration::from_secs(900))
        .unwrap();
    assert_eq!(got.n, 2);
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(300) && took < Duration::from_secs(5),
        "{took:?}"
    );
    server.join().unwrap().unwrap();
    // And the deadline, not the bound, governs the connection afterwards.
    let server = echo_once(live.clone());
    let s = d.connect(&addresses[1], Duration::from_secs(900)).unwrap();
    assert_eq!(
        s.sock.read_timeout().unwrap(),
        Some(Duration::from_secs(900))
    );
    drop(s);
    let _ = server.join().unwrap();
    drop(silent);
}

/// One dialer shared by many threads: every dial lands, and the choice ends
/// on the address that answers.
#[test]
fn a_shared_dialer_serves_many_threads() {
    let tmp = tempfile::tempdir().unwrap();
    let (server_id, client_id) = ids(tmp.path());
    let l = Arc::new(MtlsListener::bind("127.0.0.1:0", &server_id).unwrap());
    let addresses = vec![dead_port(), l.local_addr().to_string()];
    let d = Dialer::new(&client_id, Duration::from_millis(500)).unwrap();
    let (threads, dials) = (8, 5);
    let servers: Vec<_> = (0..threads * dials).map(|_| echo_once(l.clone())).collect();
    let workers: Vec<_> = (0..threads)
        .map(|_| {
            let (d, addresses) = (d.clone(), addresses.clone());
            std::thread::spawn(move || {
                for _ in 0..dials {
                    let got: Ping = d
                        .call_any(&addresses, &p(1), Duration::from_secs(5))
                        .unwrap();
                    assert_eq!(got.n, 2);
                }
            })
        })
        .collect();
    for w in workers {
        w.join().unwrap();
    }
    for s in servers {
        s.join().unwrap().unwrap();
    }
    assert_eq!(d.preferred().unwrap().entry, addresses[1]);
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

fn sa(s: &str) -> SocketAddr {
    s.parse().unwrap()
}

#[test]
fn dial_order_puts_ipv4_first_and_drops_unscoped_link_local() {
    // The shape mDNS gives back for `host.local`: link-local v6 first, v4 after.
    let got = dial::dial_order(vec![
        sa("[fe80::a2c7:5e8a:89be:46a]:7750"),
        sa("192.168.10.199:7750"),
    ]);
    assert_eq!(got, vec![sa("192.168.10.199:7750")]);

    // Global v6 is kept, but after every v4; relative order within a family
    // is preserved.
    let got = dial::dial_order(vec![
        sa("[2001:db8::1]:1"),
        sa("10.0.0.1:1"),
        sa("[fe80::1]:1"),
        sa("10.0.0.2:1"),
        sa("[2001:db8::2]:1"),
    ]);
    assert_eq!(
        got,
        vec![
            sa("10.0.0.1:1"),
            sa("10.0.0.2:1"),
            sa("[2001:db8::1]:1"),
            sa("[2001:db8::2]:1"),
        ]
    );

    // A link-local address WITH a scope is routable and stays.
    let scoped = SocketAddr::V6(std::net::SocketAddrV6::new(
        "fe80::1".parse().unwrap(),
        7750,
        0,
        4,
    ));
    assert_eq!(dial::dial_order(vec![scoped]), vec![scoped]);

    // Loopback v6 and the whole fe80::/10 block are classified by prefix.
    assert_eq!(dial::dial_order(vec![sa("[::1]:1")]), vec![sa("[::1]:1")]);
    assert_eq!(dial::dial_order(vec![sa("[febf::1]:1")]), vec![]);
    assert_eq!(
        dial::dial_order(vec![sa("[fec0::1]:1")]),
        vec![sa("[fec0::1]:1")]
    );
    assert_eq!(dial::dial_order(vec![]), vec![]);
}

#[test]
fn only_unscoped_link_local_is_named_in_the_error() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, client_id) = ids(tmp.path());
    let d = Dialer::new(&client_id, Duration::from_millis(500)).unwrap();
    let err = ping(&d, "[fe80::a2c7:5e8a:89be:46a]:7750")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("link-local IPv6 addresses without a scope"),
        "{err}"
    );
}
