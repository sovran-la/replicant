//! Two real daemons on loopback: home with its mTLS listener, a leaf dialing
//! it with home's bundle, the CLI talking to each over its Unix socket.

use super::test_certs::pair;
use super::*;
use crate::config::LeafConfig;
use crate::decide::Decision;
use crate::testutil::{self, Bed};
use std::os::unix::fs::PermissionsExt;
use wire::Enrollment;

fn local(dirs: &Dirs, req: LocalRequest) -> LocalReply {
    local::call(dirs, req).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn enroll_approve_status_dry_run_fleet_and_log_over_mtls() {
    let bed = Bed::new("");
    testutil::repo(&bed.home_hd.join("work/r"));
    let (home, leaf) = pair(&bed);

    let LocalReply::Pong { role, .. } = local(&bed.home_dirs, LocalRequest::Ping) else {
        panic!()
    };
    assert_eq!(role, "home");
    let LocalReply::Pong { role, name } = local(&bed.leaf_dirs, LocalRequest::Ping) else {
        panic!()
    };
    assert_eq!((role.as_str(), name.as_str()), ("leaf", "leaf"));

    let sub = LocalRequest::Subscribe {
        home: "home".into(),
        address: vec![],
        credentials: None,
        paths: vec![format!("{}/work", bed.leaf_hd.display())],
        as_local: None,
    };
    let LocalReply::Subscribed(out) = local(&bed.leaf_dirs, sub) else {
        panic!()
    };
    assert!(!out.enrolled, "a brand-new leaf is pending");

    let LocalReply::LeafStatus(st) = local(&bed.leaf_dirs, LocalRequest::Status) else {
        panic!()
    };
    assert_eq!(st.enrollment, Enrollment::Pending);
    assert!(st.plan.is_none());

    assert_eq!(st.pending_paths, vec!["work"]);
    assert!(
        !bed.leaf_dirs.host_identity().exists(),
        "nothing to collect yet"
    );

    let LocalReply::Pending { hosts } = local(&bed.home_dirs, LocalRequest::Pending) else {
        panic!()
    };
    assert_eq!(hosts[0].host, "leaf");
    assert_eq!(hosts[0].home_dir, bed.leaf_hd);
    let LocalReply::Approved(a) = local(
        &bed.home_dirs,
        LocalRequest::Approve {
            host: "leaf".into(),
            allow: vec![],
        },
    ) else {
        panic!()
    };
    assert_eq!(a.subscribed, vec!["work"]);
    let issued = bed.home_dirs.issued().join("leaf.pem");
    assert!(issued.exists(), "minted and waiting");

    // The leaf's next request collects its certificate over the bootstrap
    // and uses it from then on; home's copy is gone.
    let LocalReply::LeafStatus(st) = local(&bed.leaf_dirs, LocalRequest::Status) else {
        panic!()
    };
    assert_eq!(st.enrollment, Enrollment::Enrolled);
    let own = tls::Identity::load(&bed.leaf_dirs.host_identity()).unwrap();
    assert_eq!(own.fingerprint(), a.cert, "the leaf holds the pinned cert");
    assert!(!issued.exists(), "collection is one-shot");
    let mode = std::fs::metadata(bed.leaf_dirs.host_identity())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, certs::FILE_MODE);
    assert!(bed.leaf_dirs.home_bundle().exists(), "home.pem stays");
    let plan = st.plan.unwrap();
    assert_eq!(
        plan.units
            .iter()
            .find(|u| u.rel == "work/r")
            .unwrap()
            .decision,
        Decision::Pull
    );

    let LocalReply::Plan(p) = local(
        &bed.leaf_dirs,
        LocalRequest::DryRun {
            no_wait: true,
            accept_rollback: vec![],
        },
    ) else {
        panic!()
    };
    let r = p.units.iter().find(|u| u.rel == "work/r").unwrap();
    assert!(r.diff.as_ref().unwrap().files_changed > 0);
    assert!(!bed.leaf_hd.join("work/r").exists(), "dry run");

    let LocalReply::Fleet(f) = local(&bed.leaf_dirs, LocalRequest::Fleet) else {
        panic!()
    };
    let row = f.hosts.iter().find(|h| h.name == "leaf").unwrap();
    assert!(!row.stale);
    assert_eq!(row.report.as_ref().unwrap().event, "dry-run");
    assert!(f.queue.is_empty(), "the dry run released its entry");

    let LocalReply::Log { lines } = local(
        &bed.leaf_dirs,
        LocalRequest::Log {
            filter: Default::default(),
        },
    ) else {
        panic!()
    };
    let events: Vec<_> = lines.iter().map(|l| l.event.as_str()).collect();
    assert_eq!(
        events,
        ["pending", "collect", "status", "dry-run"],
        "the leaf keeps its own copy"
    );
    let LocalReply::Log { lines } = local(
        &bed.home_dirs,
        LocalRequest::Log {
            filter: Default::default(),
        },
    ) else {
        panic!()
    };
    assert!(lines.iter().any(|l| l.event == "approve"));
    let collect = lines.iter().find(|l| l.event == "collect").unwrap();
    assert_eq!(collect.host, "leaf");
    assert_eq!(collect.detail, format!("cert:{}", a.cert));

    let err = local::call(
        &bed.home_dirs,
        LocalRequest::DryRun {
            no_wait: true,
            accept_rollback: vec![],
        },
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("this machine is home"), "{err}");
    let err = local::call(&bed.leaf_dirs, LocalRequest::Pending)
        .unwrap_err()
        .to_string();
    assert!(err.contains("runs on home"), "{err}");

    leaf.stop();
    home.stop();
    assert!(!bed.leaf_dirs.socket().exists());
    assert!(
        local::call(&bed.leaf_dirs, LocalRequest::Ping)
            .unwrap_err()
            .to_string()
            .contains("replicant daemon")
    );
}

#[test]
fn subscribe_as_records_the_local_mapping_and_unsubscribe_drops_it() {
    let bed = Bed::enrolled();
    let (home, leaf) = pair(&bed);
    let work2 = bed.tmp.path().join("work2/x");
    let sub = LocalRequest::Subscribe {
        home: "home".into(),
        address: vec![],
        credentials: None,
        paths: vec!["~/work/x".into()],
        as_local: Some(work2.clone()),
    };
    let LocalReply::Subscribed(out) = local(&bed.leaf_dirs, sub) else {
        panic!()
    };
    assert!(out.enrolled);
    assert_eq!(out.absorbed, vec!["work/x"], "already covered by work");
    let cfg = LeafConfig::load(&bed.leaf_dirs.leaf()).unwrap().unwrap();
    assert_eq!(cfg.as_local["work/x"], work2);

    let LocalReply::Unsubscribed(u) = local(
        &bed.leaf_dirs,
        LocalRequest::Unsubscribe {
            paths: vec!["~/work/x".into()],
        },
    ) else {
        panic!()
    };
    assert_eq!(u.carved, vec!["work/x"]);
    assert!(
        LeafConfig::load(&bed.leaf_dirs.leaf())
            .unwrap()
            .unwrap()
            .as_local
            .is_empty()
    );

    let other_home = LocalRequest::Subscribe {
        home: "elsewhere".into(),
        address: vec![],
        credentials: None,
        paths: vec!["~/work".into()],
        as_local: None,
    };
    let err = local::call(&bed.leaf_dirs, other_home)
        .unwrap_err()
        .to_string();
    assert!(err.contains("exactly one home"), "{err}");
    leaf.stop();
    home.stop();
}

#[test]
fn a_leaf_with_another_homes_bundle_is_refused_in_the_handshake() {
    let bed = Bed::enrolled();
    let (home, leaf) = pair(&bed);
    let stranger = tempfile::tempdir().unwrap();
    let p = stranger.path();
    let theirs = certs::Paths::of(&Dirs::at(p.to_path_buf()));
    certs::ensure(&theirs).unwrap();
    std::fs::copy(&theirs.bootstrap, bed.leaf_dirs.home_bundle()).unwrap();
    std::fs::copy(&theirs.bootstrap, bed.leaf_dirs.host_identity()).unwrap();
    let err = local::call(&bed.leaf_dirs, LocalRequest::Status)
        .unwrap_err()
        .to_string();
    assert!(err.contains("refused the handshake"), "{err}");
    leaf.stop();
    home.stop();
}

#[test]
fn one_daemon_per_directory() {
    let bed = Bed::new("");
    let leaf = Daemon::start(bed.leaf_dirs.clone()).unwrap();
    let err = Daemon::start(bed.leaf_dirs.clone())
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("already running"), "{err}");
    leaf.stop();
    // The lock is released when the daemon drops it. Under a parallel test
    // run another test's child process can hold a forked copy of the fd for
    // the instant between fork and exec, so allow a moment rather than flake.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let again = loop {
        match Daemon::start(bed.leaf_dirs.clone()) {
            Ok(d) => break d,
            Err(e) if std::time::Instant::now() < deadline => {
                assert!(e.to_string().contains("already running"), "{e}");
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("the lock was never released: {e}"),
        }
    };
    again.stop();
}

#[test]
fn home_without_listen_refuses_to_start() {
    let bed = Bed::new("");
    let text = std::fs::read_to_string(bed.home_dirs.fleet()).unwrap();
    std::fs::write(
        bed.home_dirs.fleet(),
        text.replace("listen = \"127.0.0.1:0\"\n", ""),
    )
    .unwrap();
    let err = Daemon::start(bed.home_dirs.clone())
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("[daemon] listen"), "{err}");
}

/// `stop` with a bound: a listener that is not woken shows up as a failure
/// here, not as a test that never ends.
fn stop_within(d: Daemon, limit: std::time::Duration) {
    let t = std::thread::spawn(move || d.stop());
    let deadline = std::time::Instant::now() + limit;
    while !t.is_finished() {
        assert!(std::time::Instant::now() < deadline, "stop did not finish");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    t.join().unwrap();
}

#[test]
fn home_listens_on_every_listed_address_and_a_leaf_falls_back_to_one_that_answers() {
    // Two distinct loopback listeners need v6 loopback; without it there is
    // nothing to test here (a repeated address is one listener).
    if std::net::TcpListener::bind("[::1]:0").is_err() {
        eprintln!("skipped: no IPv6 loopback");
        return;
    }
    let bed = Bed::enrolled();
    testutil::hand_edit(
        &bed.home_dirs.fleet(),
        "listen = \"127.0.0.1:0\"",
        "listen = [\"127.0.0.1:0\", \"[::1]:0\"]",
    );
    let home = Daemon::start(bed.home_dirs.clone()).unwrap();
    let addrs = home.tcp_addrs();
    assert_eq!(addrs.len(), 2, "{addrs:?}");
    assert!(addrs[0].is_ipv4() && addrs[1].is_ipv6(), "{addrs:?}");
    assert_eq!(home.tcp_addr(), Some(addrs[0]));

    // The leaf lists a dead address first, then home's second listener.
    let dead = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .to_string();
    super::test_certs::equip_leaf(&bed);
    super::test_certs::leaf_toml(&bed, &dead);
    let mut cfg = LeafConfig::load(&bed.leaf_dirs.leaf()).unwrap().unwrap();
    cfg.home.address =
        crate::config::Addresses::new(vec![dead.clone(), addrs[1].to_string()]).unwrap();
    cfg.save(&bed.leaf_dirs.leaf()).unwrap();
    let saved = std::fs::read_to_string(bed.leaf_dirs.leaf()).unwrap();
    assert!(saved.contains("address = ["), "{saved}");

    let leaf = Daemon::start(bed.leaf_dirs.clone()).unwrap();
    let LocalReply::LeafStatus(st) = local(&bed.leaf_dirs, LocalRequest::Status) else {
        panic!()
    };
    assert_eq!(st.enrollment, Enrollment::Enrolled);
    stop_within(leaf, std::time::Duration::from_secs(10));
    stop_within(home, std::time::Duration::from_secs(10));
}

/// Start a daemon, retrying for a moment while the lock reads as held: a
/// child being spawned by another test inherits the lock file until its
/// exec closes it, so right after a close `flock` can still see it busy
/// (review A23: thousands of false busies per 300k with spawns beside it,
/// none without). Production never starts a second daemon in-process, so
/// `lock()` itself stays immediate: `daemon_tests.rs` relies on that.
fn start_retrying(dirs: &Dirs) -> Result<Daemon> {
    let by = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        match Daemon::start(dirs.clone()) {
            Err(e)
                if e.to_string().contains("already running") && std::time::Instant::now() < by =>
            {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            r => return r,
        }
    }
}

/// One listed address that cannot bind fails the start outright: all or
/// none, and the one that did bind is released. On IPv4 alone, so it runs
/// everywhere.
#[test]
fn home_with_one_unbindable_listen_address_does_not_start() {
    let bed = Bed::new("");
    // A real port for the first entry, found by binding and releasing it:
    // only then can "nothing was left bound" be told from "a fresh port".
    let first = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let both = format!(
        "listen = [\"{first}\", \"{}\"]",
        taken.local_addr().unwrap()
    );
    testutil::hand_edit(&bed.home_dirs.fleet(), "listen = \"127.0.0.1:0\"", &both);
    let err = start_retrying(&bed.home_dirs).err().unwrap().to_string();
    assert!(err.contains("could not bind"), "{err}");
    // The first address bound and was then released by the failed start: a
    // home listing only it binds the very same port.
    testutil::hand_edit(
        &bed.home_dirs.fleet(),
        &both,
        &format!("listen = \"{first}\""),
    );
    let home = start_retrying(&bed.home_dirs).unwrap();
    assert_eq!(home.tcp_addrs(), vec![first]);
    stop_within(home, std::time::Duration::from_secs(10));
}

#[test]
fn leaf_without_a_home_explains_how_to_get_one() {
    let bed = Bed::new("");
    let leaf = Daemon::start(bed.leaf_dirs.clone()).unwrap();
    let err = local::call(&bed.leaf_dirs, LocalRequest::Status)
        .unwrap_err()
        .to_string();
    assert!(err.contains("replicant subscribe"), "{err}");
    leaf.stop();
}

/// `subscribe --address` (repeated) writes the list as given, trimmed and
/// deduplicated, and tells the CLI where home is reached now; without it
/// the saved list stays; one `--address` that shrinks the list says so; a
/// blank one is refused before anything is written.
#[test]
fn subscribe_address_writes_the_list_and_reports_what_it_replaced() {
    let bed = Bed::enrolled();
    let (home, leaf) = pair(&bed);
    let live = home.tcp_addr().unwrap().to_string();
    let dead = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .to_string();
    let sub = |address: Vec<String>| LocalRequest::Subscribe {
        home: "home".into(),
        address,
        credentials: None,
        paths: vec![format!("{}/work", bed.leaf_hd.display())],
        as_local: None,
    };

    // The dead one first, then home: the request still goes through, and
    // the list is saved as given, with the stray duplicate and spaces gone.
    let LocalReply::Subscribed(out) = local(
        &bed.leaf_dirs,
        sub(vec![format!(" {dead} "), live.clone(), dead.clone()]),
    ) else {
        panic!()
    };
    assert_eq!(out.addresses, vec![dead.clone(), live.clone()]);
    assert!(out.replaced.is_empty(), "{:?}", out.replaced);
    let cfg = LeafConfig::load(&bed.leaf_dirs.leaf()).unwrap().unwrap();
    assert_eq!(
        cfg.home.address,
        crate::config::Addresses::new(vec![dead.clone(), live.clone()]).unwrap()
    );
    let text = std::fs::read_to_string(bed.leaf_dirs.leaf()).unwrap();
    assert!(text.contains("address = ["), "{text}");

    // No --address: the list is kept and still reported.
    let LocalReply::Subscribed(out) = local(&bed.leaf_dirs, sub(vec![])) else {
        panic!()
    };
    assert_eq!(out.addresses, vec![dead.clone(), live.clone()]);
    assert!(out.replaced.is_empty());

    // One --address replaces the list, and the reply names what it replaced.
    let LocalReply::Subscribed(out) = local(&bed.leaf_dirs, sub(vec![live.clone()])) else {
        panic!()
    };
    assert_eq!(out.addresses, vec![live.clone()]);
    assert_eq!(out.replaced, vec![dead.clone(), live.clone()]);
    let rendered = crate::cli::render::subscribed(&out);
    assert!(
        rendered.contains(&format!("home is reached at: {live}"))
            && rendered.contains("--address replaced the saved list"),
        "{rendered}"
    );
    let text = std::fs::read_to_string(bed.leaf_dirs.leaf()).unwrap();
    assert!(text.contains(&format!("address = \"{live}\"")), "{text}");

    // A blank address is refused, and the file is untouched.
    let err = local::call(&bed.leaf_dirs, sub(vec!["  ".into()]))
        .unwrap_err()
        .to_string();
    assert!(err.contains("--address") && err.contains("empty"), "{err}");
    assert_eq!(std::fs::read_to_string(bed.leaf_dirs.leaf()).unwrap(), text);

    // Two saved again, then one --address that cannot reach home: the list
    // is still replaced (a home that is down can be named now), and the
    // error says what it replaced, so the fallback is not lost in silence
    // (review A29).
    let LocalReply::Subscribed(_) = local(&bed.leaf_dirs, sub(vec![dead.clone(), live.clone()]))
    else {
        panic!()
    };
    let err = local::call(&bed.leaf_dirs, sub(vec![dead.clone()]))
        .unwrap_err()
        .to_string();
    assert!(err.contains("unreachable"), "{err}");
    assert!(
        err.contains(&format!(
            "note: --address replaced the saved list ({dead}, {live})"
        )),
        "{err}"
    );
    let cfg = LeafConfig::load(&bed.leaf_dirs.leaf()).unwrap().unwrap();
    assert_eq!(
        cfg.home.address,
        crate::config::Addresses::one(dead.clone())
    );

    leaf.stop();
    home.stop();
}
