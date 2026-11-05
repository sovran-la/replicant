//! Data connections on home's mTLS listener: admitted like a request (a
//! pinned host's own certificate), only for the head of the queue under a
//! sync's entry, for a unit home serves, and no more of them per unit than
//! `[daemon] streams`. And home's manifest of a unit over a request.

use super::test_certs::{dial, leaf_identity};
use super::wire::{HomeReply, HomeRequest};
use super::*;
use crate::marker::Direction;
use crate::native::frame::{Frame, Kind};
use crate::testutil::{self, Bed, hello};

fn home(bed: &Bed) -> Daemon {
    Daemon::start(bed.home_dirs.clone()).unwrap()
}

fn lease(leaf: &super::test_certs::RawLeaf, apply: bool) -> u64 {
    match leaf
        .call(HomeRequest::Acquire {
            no_wait: true,
            apply,
        })
        .unwrap()
    {
        HomeReply::Lease { lease, .. } => lease,
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_head_of_the_queue_gets_a_data_connection_and_nobody_else_does() {
    let bed = Bed::enrolled();
    testutil::write(&bed.home_hd.join("work/a.txt"), "x");
    let d = home(&bed);
    let leaf = dial(&d, &leaf_identity(&bed, "leaf"), "leaf", &bed.leaf_hd);

    let refused = |h| {
        leaf.data(&h)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default()
    };
    let none = refused(hello(77, "work", Direction::Pull));
    assert!(none.contains("does not hold the head"), "{none}");

    let id = lease(&leaf, true);
    let mut c = leaf.data(&hello(id, "work", Direction::Pull)).unwrap();
    c.send(&Frame::new(Kind::Done, vec![])).unwrap();

    let outside = refused(hello(id, "elsewhere", Direction::Pull));
    assert!(
        outside.contains("not under anything home serves"),
        "{outside}"
    );
    let dotdot = refused(hello(id, "work/../x", Direction::Pull));
    assert!(dotdot.contains("work/../x"), "{dotdot}");
    let unbegun = refused(hello(id, "work", Direction::Push));
    assert!(
        unbegun.contains("no push of work by leaf has begun"),
        "{unbegun}"
    );
    let mut other = hello(id, "work", Direction::Pull);
    other.host = "other".into();
    let named = refused(other);
    assert!(named.contains("a host acts only as itself"), "{named}");
    let mut old = hello(id, "work", Direction::Pull);
    old.version = 0;
    assert!(refused(old).contains("data protocol 0"));

    leaf.call(HomeRequest::Release { lease: id }).unwrap();
    let dry = lease(&leaf, false);
    let reads = refused(hello(dry, "work", Direction::Pull));
    assert!(reads.contains("writes nothing"), "{reads}");
    d.stop();
}

#[test]
fn the_bootstrap_and_unpinned_certificates_get_no_data_connection() {
    let bed = Bed::enrolled();
    let d = home(&bed);
    let p = certs::Paths::of(&bed.home_dirs);
    let boot = dial(&d, &p.bootstrap, "leaf", &bed.leaf_hd);
    let err = boot
        .data(&hello(1, "work", Direction::Pull))
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains(home::BOOTSTRAP_ONLY), "{err}");
    let stranger = dial(
        &d,
        &leaf_identity(&bed, "stranger"),
        "stranger",
        &bed.leaf_hd,
    );
    let mut h = hello(1, "work", Direction::Pull);
    h.host = "stranger".into();
    let err = stranger.data(&h).err().unwrap().to_string();
    assert!(err.contains(home::UNKNOWN_CERT), "{err}");
    d.stop();
}

#[test]
fn no_more_data_connections_per_unit_than_home_allows() {
    let bed = Bed::enrolled();
    let fleet = bed.home_dirs.fleet();
    testutil::hand_edit(&fleet, "listen = ", "streams = 2\nlisten = ");
    let d = home(&bed);
    let leaf = dial(&d, &leaf_identity(&bed, "leaf"), "leaf", &bed.leaf_hd);
    let id = lease(&leaf, true);
    let a = leaf.data(&hello(id, "work", Direction::Pull)).unwrap();
    let b = leaf.data(&hello(id, "work", Direction::Pull)).unwrap();
    let err = leaf
        .data(&hello(id, "work", Direction::Pull))
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("2 data connections per unit"), "{err}");
    let mut b = b;
    b.send(&Frame::new(Kind::Done, vec![])).unwrap();
    // The slot comes back once home has seen the connection end.
    let mut ok = None;
    for _ in 0..100 {
        if let Ok(c) = leaf.data(&hello(id, "work", Direction::Pull)) {
            ok = Some(c);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(ok.is_some(), "a closed connection's slot is given back");
    drop(a);
    d.stop();
}

#[test]
fn a_connection_that_is_neither_request_nor_data_is_dropped_and_home_serves_on() {
    use std::io::{Read, Write};
    let bed = Bed::enrolled();
    let d = home(&bed);
    let id = leaf_identity(&bed, "leaf");
    let leaf = dial(&d, &id, "leaf", &bed.leaf_hd);
    let ident = super::tls::Identity::load(&id).unwrap();
    let dialer = super::remote::Dialer::new(&ident, std::time::Duration::from_secs(5)).unwrap();
    let addr = d.tcp_addr().unwrap().to_string();
    let t = std::time::Duration::from_secs(5);

    let mut raw = dialer.connect(&addr, t).unwrap();
    raw.write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
    let mut buf = [0u8; 16];
    assert_eq!(raw.read(&mut buf).unwrap_or(0), 0, "dropped without a word");

    // The magic line, then garbage where the hello should be: refused in a
    // frame, then closed.
    let mut c = dialer.open_data(&addr, t).unwrap();
    c.io().write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
    let f = c.recv().unwrap();
    assert_eq!(f.kind, Kind::Refused);
    assert!(
        f.parse::<String>()
            .unwrap()
            .contains("larger than the limit")
    );

    assert!(matches!(
        leaf.call(HomeRequest::Subscriptions).unwrap(),
        HomeReply::Subscriptions(_)
    ));
    d.stop();
}

/// A data connection falls through a dead listed address like a request
/// does: the walk is in the dialer, under both.
#[test]
fn a_data_connection_falls_through_to_the_address_that_answers() {
    let bed = Bed::enrolled();
    let d = home(&bed);
    let id = leaf_identity(&bed, "leaf");
    let ident = super::tls::Identity::load(&id).unwrap();
    let dialer = super::remote::Dialer::new(&ident, std::time::Duration::from_millis(500)).unwrap();
    let dead = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .to_string();
    let live = d.tcp_addr().unwrap().to_string();
    let t = std::time::Duration::from_secs(5);

    let mut c = dialer
        .open_data_any(&[dead.clone(), live.clone()], t)
        .unwrap();
    assert_eq!(dialer.preferred().unwrap().entry, live);
    // It is a real data connection: home answers garbage with a refusal frame.
    c.io().write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
    assert_eq!(c.recv().unwrap().kind, Kind::Refused);

    // Only the dead one listed: the data path reports it like a request would.
    let err = match dialer.open_data_any(std::slice::from_ref(&dead), t) {
        Ok(_) => panic!("a dead address answered"),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains(&format!("home at {dead} is unreachable")),
        "{err}"
    );
    d.stop();
}

#[test]
fn homes_manifest_of_a_unit_needs_the_head_and_honours_every_exclude() {
    let bed = Bed::enrolled();
    let w = bed.home_hd.join("work");
    testutil::write(&w.join("a.txt"), "x");
    testutil::write(&w.join("target/junk"), "root exclude");
    testutil::write(&w.join("r/README"), "a repo the loose unit leaves out");
    testutil::write(&w.join("wt/f"), "a worktree");
    testutil::write(&w.join(".env"), "ignored on the leaf's side");
    let d = home(&bed);
    let leaf = dial(&d, &leaf_identity(&bed, "leaf"), "leaf", &bed.leaf_hd);
    let ask = |lease| HomeRequest::Manifest {
        lease,
        rel: "work".into(),
        kind: crate::units::UnitKind::Loose,
        is_file: false,
        union: vec!["work/r".into(), "elsewhere/x".into()],
        worktrees: vec!["work/wt".into()],
        ignored: vec![".env".into(), "../escape".into()],
    };
    let err = leaf.refused(ask(5));
    assert!(err.contains("does not hold the head"), "{err}");
    let dry = lease(&leaf, false);
    let HomeReply::Manifest { manifest } = leaf.call(ask(dry)).unwrap() else {
        panic!()
    };
    let rels: Vec<&str> = manifest.entries.iter().map(|e| e.rel.as_str()).collect();
    assert_eq!(rels, ["a.txt"]);
    d.stop();
}

#[test]
fn an_in_process_link_serves_its_data_connection_on_the_callers_thread() {
    use crate::plan::HomeLink;
    let bed = Bed::enrolled();
    let h = bed.home();
    let link = testutil::InProcess {
        home: &h,
        host: "leaf".into(),
        home_dir: bed.leaf_hd.clone(),
    };
    let id = match link
        .call(HomeRequest::Acquire {
            no_wait: true,
            apply: true,
        })
        .unwrap()
    {
        HomeReply::Lease { lease, .. } => lease,
        other => panic!("{other:?}"),
    };
    let data = link.open_data("work").unwrap();
    let mut conn = data.conn;
    std::thread::scope(|s| {
        s.spawn(data.serve.expect("home's side, in-process"));
        crate::native::session::open(&mut conn, &hello(id, "work", Direction::Pull)).unwrap();
        conn.send(&Frame::new(Kind::Done, vec![])).unwrap();
    });
}

/// Over the mTLS listener too, the version is judged before the request is
/// read: a leaf of another version, whose `HomeCall` may be shaped
/// differently, hears which versions disagree (review A32). And home's
/// bytes for a subscribe outcome carry no empty `addresses`/`replaced`,
/// so a 0.4.4 leaf reads them unchanged (review X4).
#[test]
fn a_mis_shaped_request_over_mtls_is_refused_by_version() {
    use std::io::Write;
    let bed = Bed::enrolled();
    let d = home(&bed);
    let id = leaf_identity(&bed, "leaf");
    let ident = super::tls::Identity::load(&id).unwrap();
    let dialer = super::remote::Dialer::new(&ident, std::time::Duration::from_secs(5)).unwrap();
    let addr = d.tcp_addr().unwrap().to_string();
    let t = std::time::Duration::from_secs(5);

    let answer_to = |line: &str| -> super::wire::HomeAnswer {
        let mut s = dialer.connect(&addr, t).unwrap();
        s.write_all(line.as_bytes()).unwrap();
        super::remote::recv(&mut s).unwrap()
    };
    // 0.4.2 shape, another version: version first.
    let old = format!(
        "{{\"version\":{},\"host\":\"leaf\",\"home_dir\":\"{}\",\"request\":{{\"Nonsense\":1}}}}\n",
        super::wire::WIRE_VERSION - 1,
        bed.leaf_hd.display()
    );
    let err = answer_to(&old).result.unwrap_err();
    assert!(
        err.contains(&format!("wire version {}", super::wire::WIRE_VERSION - 1))
            && err.contains("run the same replicant on both"),
        "{err}"
    );
    assert!(!err.contains("unreadable"), "{err}");
    // The same shape claiming this version is a field problem.
    let same = old.replace(
        &format!("\"version\":{}", super::wire::WIRE_VERSION - 1),
        &format!("\"version\":{}", super::wire::WIRE_VERSION),
    );
    let err = answer_to(&same).result.unwrap_err();
    assert!(err.contains("unreadable message"), "{err}");

    let empty = super::wire::HomeAnswer {
        result: Ok(HomeReply::Subscribed(super::wire::SubscribeOutcome {
            enrolled: true,
            home: "h".into(),
            requested: vec!["a".into()],
            paths: vec!["a".into()],
            absorbed: vec![],
            addresses: vec![],
            replaced: vec![],
        })),
        journal: vec![],
    };
    let bytes = serde_json::to_string(&empty).unwrap();
    assert!(
        !bytes.contains("addresses") && !bytes.contains("replaced"),
        "{bytes}"
    );
    let back: super::wire::HomeAnswer = serde_json::from_str(&bytes).unwrap();
    assert!(
        matches!(back.result, Ok(HomeReply::Subscribed(o)) if o.addresses.is_empty() && o.replaced.is_empty() && o.paths == vec!["a".to_string()])
    );
    d.stop();
}
