//! Two real daemons on loopback: home with its mTLS listener, a leaf dialing
//! it with home's bundle, the CLI talking to each over its Unix socket.

use super::*;
use crate::config::{HomeEntry, LeafConfig, leaf::LOCAL_RSYNC};
use crate::decide::Decision;
use crate::testutil::{self, Bed};
use wire::Enrollment;

fn leaf_toml(bed: &Bed, address: &str) {
    LeafConfig {
        name: Some("leaf".into()),
        home_dir: Some(bed.leaf_hd.clone()),
        home: HomeEntry {
            name: "home".into(),
            address: address.into(),
            credentials: None,
            rsync_host: Some(LOCAL_RSYNC.into()),
            rsync_timeout_secs: None,
        },
        as_local: Default::default(),
    }
    .save(&bed.leaf_dirs.leaf())
    .unwrap();
}

/// Home and leaf daemons, the leaf holding a client identity of its own.
fn pair(bed: &Bed) -> (Daemon, Daemon) {
    let home = Daemon::start(bed.home_dirs.clone()).unwrap();
    let addr = home.tcp_addr().unwrap().to_string();
    let id = super::test_certs::leaf_identity(bed, "leaf");
    std::fs::copy(id, bed.leaf_dirs.home_bundle()).unwrap();
    leaf_toml(bed, &addr);
    let leaf = Daemon::start(bed.leaf_dirs.clone()).unwrap();
    (home, leaf)
}

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
        address: None,
        rsync_host: None,
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

    let LocalReply::LeafStatus(st) = local(&bed.leaf_dirs, LocalRequest::Status) else {
        panic!()
    };
    assert_eq!(st.enrollment, Enrollment::Enrolled);
    let plan = st.plan.unwrap();
    assert_eq!(
        plan.units
            .iter()
            .find(|u| u.rel == "work/r")
            .unwrap()
            .decision,
        Decision::Pull
    );

    let LocalReply::Plan(p) = local(&bed.leaf_dirs, LocalRequest::DryRun { no_wait: true }) else {
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
        ["pending", "status", "dry-run"],
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

    let err = local::call(&bed.home_dirs, LocalRequest::DryRun { no_wait: true })
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
        address: None,
        rsync_host: None,
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
        address: None,
        rsync_host: None,
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
