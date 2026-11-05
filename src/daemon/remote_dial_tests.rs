//! Dialing home over one address or several: fallback, stickiness, how each
//! kind of failure is told apart and reported. The address order within one
//! name is `remote_dial_order_tests.rs`.

use super::super::tests::{Ping, echo_once, ids, ping};
use super::super::{HANDSHAKE_TIMEOUT, MtlsListener};
use super::*;
use std::net::TcpListener;

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
    // One line per address; the errno text is the OS's and a freed port can
    // be rebound by a parallel test, so only the shape is asserted.
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
    // Bounded: a dial that used the deadline would sit for 900 s, and this
    // must fail by name, not hang the suite.
    let got: Ping = {
        let (d, addresses) = (d.clone(), addresses.clone());
        crate::native::tests_fixture::bounded(Duration::from_secs(20), move || {
            d.call_any(&addresses, &p(1), Duration::from_secs(900))
                .unwrap()
        })
    };
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
    assert_eq!(
        s.sock.write_timeout().unwrap(),
        Some(Duration::from_secs(900))
    );
    // The bound a real dialer starts with is home's own handshake bound.
    assert_eq!(
        Dialer::new(&client_id, Duration::from_secs(5))
            .unwrap()
            .handshake_timeout(),
        HANDSHAKE_TIMEOUT
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

/// What each failure is called: only a TLS verdict blames the bundle. A
/// silent peer, one that closes during the handshake, and a mix of silent
/// and wrong are all the network's fault, so they are `Remote` errors.
#[test]
fn handshake_failures_are_named_by_what_judged_them() {
    let (home, stranger) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (_, client_id) = ids(home.path());
    let stranger_id = ids(stranger.path()).0;
    let d = Dialer::new(&client_id, Duration::from_secs(5))
        .unwrap()
        .with_handshake_timeout(Duration::from_millis(300));

    // Accepts TCP, says nothing.
    let silent = TcpListener::bind("127.0.0.1:0").unwrap();
    let silent_addr = silent.local_addr().unwrap().to_string();
    let err = ping(&d, &silent_addr).unwrap_err();
    assert!(matches!(err, Error::Remote(_)), "{err}");
    assert!(
        err.to_string()
            .contains("no answer to the handshake within 300ms"),
        "{err}"
    );

    // Accepts TCP, then closes.
    let closer = TcpListener::bind("127.0.0.1:0").unwrap();
    let closer_addr = closer.local_addr().unwrap().to_string();
    let t = std::thread::spawn(move || drop(closer.accept().unwrap()));
    let err = ping(&d, &closer_addr).unwrap_err();
    t.join().unwrap();
    assert!(matches!(err, Error::Remote(_)), "{err}");
    assert!(
        err.to_string().contains("lost during the handshake"),
        "{err}"
    );

    // A stranger: that one is a refusal, as a single address and as the
    // only refusal beside a silent one.
    let wrong = Arc::new(MtlsListener::bind("127.0.0.1:0", &stranger_id).unwrap());
    let wrong_addr = wrong.local_addr().to_string();
    let r = echo_once(wrong.clone());
    let err = ping(&d, &wrong_addr).unwrap_err();
    assert!(r.join().unwrap().is_err());
    assert!(matches!(err, Error::Tls(_)), "{err}");
    let r = echo_once(wrong.clone());
    let err = d
        .call_any::<_, Ping>(
            &[silent_addr.clone(), wrong_addr.clone()],
            &p(0),
            Duration::from_secs(5),
        )
        .unwrap_err();
    assert!(r.join().unwrap().is_err());
    assert!(
        matches!(err, Error::Remote(_)),
        "a silent one is not a refusal: {err}"
    );
    let text = err.to_string();
    assert!(
        text.contains("did not answer at any address") && !text.contains("current bundle"),
        "{text}"
    );
    drop(silent);
}

/// A failed walk that started before a choice was made (it saw `None`)
/// must not erase it; one that started with this choice does.
#[test]
fn forget_is_conditional_on_what_the_walk_saw() {
    let tmp = tempfile::tempdir().unwrap();
    let d = Dialer::new(&ids(tmp.path()).1, Duration::from_secs(1)).unwrap();
    let l = Arc::new(MtlsListener::bind("127.0.0.1:0", &ids(tmp.path()).0).unwrap());
    let live = vec![l.local_addr().to_string()];
    let server = echo_once(l.clone());
    let _: Ping = d.call_any(&live, &p(1), Duration::from_secs(5)).unwrap();
    server.join().unwrap().unwrap();
    let chosen = d.preferred();
    assert!(chosen.is_some());
    d.forget(&None);
    assert_eq!(d.preferred(), chosen);
    d.forget(&chosen);
    assert_eq!(d.preferred(), None);
}
