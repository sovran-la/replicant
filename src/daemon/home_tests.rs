use super::*;
use crate::testutil::{self, Bed};
use std::path::PathBuf;

fn call(home: &Home, host: &str, hd: &std::path::Path, request: HomeRequest) -> HomeAnswer {
    home.handle(HomeCall {
        version: WIRE_VERSION,
        host: host.into(),
        home_dir: hd.to_path_buf(),
        request,
    })
}

fn ok(a: HomeAnswer) -> HomeReply {
    a.result.unwrap_or_else(|e| panic!("refused: {e}"))
}

#[test]
fn pending_host_flow_end_to_end() {
    let bed = Bed::new("");
    let home = bed.home();
    let raw = vec![format!("{}/work/tundra", bed.leaf_hd.display())];
    let a = call(
        &home,
        "newbox",
        &bed.leaf_hd,
        HomeRequest::Subscribe { paths: raw },
    );
    assert_eq!(a.journal.len(), 1, "home tells the leaf what it journaled");
    assert!(
        a.journal[0].contains("\tpending\tnewbox\twork/tundra\t"),
        "{}",
        a.journal[0]
    );
    let HomeReply::Subscribed(out) = ok(a) else {
        panic!()
    };
    assert!(!out.enrolled);

    let HomeReply::Subscriptions(v) = ok(call(
        &home,
        "newbox",
        &bed.leaf_hd,
        HomeRequest::Subscriptions,
    )) else {
        panic!()
    };
    assert_eq!(v.enrollment, Enrollment::Pending);
    assert_eq!(v.pending_paths, vec!["work/tundra"]);
    assert!(v.specs.is_empty(), "a pending host is served nothing");

    let st = call(
        &home,
        "newbox",
        &bed.leaf_hd,
        HomeRequest::State {
            leaf_repos: vec![],
            case_check: false,
        },
    );
    assert!(st.result.unwrap_err().contains("pending approval"));
    let acq = call(
        &home,
        "newbox",
        &bed.leaf_hd,
        HomeRequest::Acquire { no_wait: true },
    );
    assert!(
        acq.result.is_err(),
        "a pending host cannot take a queue entry"
    );

    assert_eq!(home.pending()[0].host, "newbox");
    let prev = home
        .preview_allow("newbox", &["~/work/tundra".into()])
        .unwrap();
    assert_eq!(prev.allow, Some(vec!["work/tundra".to_string()]));
    let approved = home.approve("newbox", &["~/work/tundra".into()]).unwrap();
    assert_eq!(approved.subscribed, vec!["work/tundra"]);

    let HomeReply::Subscriptions(v) = ok(call(
        &home,
        "newbox",
        &bed.leaf_hd,
        HomeRequest::Subscriptions,
    )) else {
        panic!()
    };
    assert_eq!(v.enrollment, Enrollment::Enrolled);
    assert_eq!(v.specs[0].rel, "work/tundra");
    assert_eq!(v.specs[0].exclude, vec!["target"]);
    let log = home.log(&Filter::default()).unwrap();
    let events: Vec<_> = log.iter().map(|l| l.event.as_str()).collect();
    assert_eq!(events, ["pending", "approve"]);
    assert_eq!(log[1].detail, "allow:work/tundra");
}

#[test]
fn unknown_host_is_unknown_and_refused_state() {
    let bed = Bed::new("");
    let home = bed.home();
    let HomeReply::Subscriptions(v) = ok(call(
        &home,
        "ghost",
        &bed.leaf_hd,
        HomeRequest::Subscriptions,
    )) else {
        panic!()
    };
    assert_eq!(v.enrollment, Enrollment::Unknown);
    let st = call(
        &home,
        "ghost",
        &bed.leaf_hd,
        HomeRequest::State {
            leaf_repos: vec![],
            case_check: false,
        },
    );
    assert!(st.result.unwrap_err().contains("not enrolled"));
}

#[test]
fn wire_version_host_name_and_home_dir_are_checked() {
    let bed = Bed::enrolled();
    let home = bed.home();
    let a = home.handle(HomeCall {
        version: 99,
        host: "leaf".into(),
        home_dir: bed.leaf_hd.clone(),
        request: HomeRequest::Subscriptions,
    });
    assert!(a.result.unwrap_err().contains("wire version 99"));
    for bad in ["Leaf", "le af", "../x", ""] {
        let a = call(&home, bad, &bed.leaf_hd, HomeRequest::Subscriptions);
        assert!(a.result.is_err(), "{bad:?} accepted");
    }
    let a = call(
        &home,
        "leaf",
        std::path::Path::new("rel"),
        HomeRequest::Subscriptions,
    );
    assert!(a.result.is_err());
}

#[test]
fn cap_narrowed_after_subscribing_bounds_what_is_served() {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/a/r"));
    testutil::repo(&bed.home_hd.join("work/b/r"));
    let text = std::fs::read_to_string(bed.home_dirs.fleet()).unwrap();
    std::fs::write(
        bed.home_dirs.fleet(),
        text.replace(
            "[subscriptions.leaf]\npaths = [\"work\"]",
            "[subscriptions.leaf]\npaths = [\"work/a\", \"work/b\"]",
        ),
    )
    .unwrap();
    let mut text = std::fs::read_to_string(bed.home_dirs.fleet()).unwrap();
    text = text.replace("[hosts.leaf]\n", "[hosts.leaf]\nallow = [\"work/a\"]\n");
    std::fs::write(bed.home_dirs.fleet(), text).unwrap();
    let home = bed.home();

    let HomeReply::Subscriptions(v) = ok(call(
        &home,
        "leaf",
        &bed.leaf_hd,
        HomeRequest::Subscriptions,
    )) else {
        panic!()
    };
    assert_eq!(
        v.specs.iter().map(|s| s.rel.as_str()).collect::<Vec<_>>(),
        ["work/a"]
    );
    assert_eq!(v.refused.len(), 1);
    assert_eq!(v.refused[0].unit, "work/b");

    let HomeReply::State(s) = ok(call(
        &home,
        "leaf",
        &bed.leaf_hd,
        HomeRequest::State {
            leaf_repos: vec!["work/b/r".into(), "work/../etc".into(), "other/x".into()],
            case_check: false,
        },
    )) else {
        panic!()
    };
    assert!(
        s.units.iter().all(|u| u.rel.starts_with("work/a")),
        "{:?}",
        s.units
    );
    assert_eq!(s.repos, vec!["work/a/r"]);
}

#[test]
fn state_unions_leaf_repos_inside_bounds_and_reports_markers() {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/h"));
    let m = crate::marker::Marker {
        version: crate::record::RECORD_VERSION,
        rel: "work/h".into(),
        direction: crate::marker::Direction::Push,
        host: "other".into(),
        source_state: crate::state::UnitState::Empty,
        started_at: 1,
    };
    crate::marker::store(&bed.home_dirs.state())
        .put(&m)
        .unwrap();
    let home = bed.home();
    let HomeReply::State(s) = ok(call(
        &home,
        "leaf",
        &bed.leaf_hd,
        HomeRequest::State {
            leaf_repos: vec!["work/l".into()],
            case_check: true,
        },
    )) else {
        panic!()
    };
    let rels: Vec<_> = s.units.iter().map(|u| u.rel.as_str()).collect();
    assert_eq!(rels, ["work/h", "work/l", "work"]);
    let l = s.units.iter().find(|u| u.rel == "work/l").unwrap();
    assert!(l.state.is_empty(), "the leaf's repo does not exist on home");
    assert_eq!(s.markers, vec![m]);
    assert_eq!(s.home_dir, bed.home_hd);
}

#[test]
fn queue_through_the_handler_and_expiry_is_journaled() {
    let bed = Bed::enrolled();
    let text = std::fs::read_to_string(bed.home_dirs.fleet()).unwrap();
    std::fs::write(
        bed.home_dirs.fleet(),
        text.replace("listen", "lease_secs = 1\nlisten"),
    )
    .unwrap();
    let home = bed.home();
    let HomeReply::Lease { lease, lease_secs } = ok(call(
        &home,
        "leaf",
        &bed.leaf_hd,
        HomeRequest::Acquire { no_wait: true },
    )) else {
        panic!()
    };
    assert_eq!(lease_secs, 1);
    let busy = call(
        &home,
        "leaf",
        &bed.leaf_hd,
        HomeRequest::Acquire { no_wait: true },
    );
    assert!(busy.result.unwrap_err().contains("queue busy"));
    ok(call(
        &home,
        "leaf",
        &bed.leaf_hd,
        HomeRequest::Heartbeat { lease },
    ));
    std::thread::sleep(Duration::from_millis(1100));
    home.reap();
    assert!(home.queue.view().is_empty());
    let log = home.log(&Filter::default()).unwrap();
    assert_eq!(log.last().unwrap().event, "expired");
    assert!(log.last().unwrap().detail.contains("markers untouched"));
    let gone = call(&home, "leaf", &bed.leaf_hd, HomeRequest::Release { lease });
    assert!(gone.result.is_err());
}

#[test]
fn fleet_state_shows_stale_and_fresh_leaves() {
    let bed = Bed::enrolled();
    let text = std::fs::read_to_string(bed.home_dirs.fleet()).unwrap();
    let extra = format!(
        "\n[hosts.spruce]\nhome_dir = \"{}\"\n",
        bed.leaf_hd.display()
    );
    std::fs::write(bed.home_dirs.fleet(), text + &extra).unwrap();
    let home = bed.home();
    ok(call(
        &home,
        "leaf",
        &bed.leaf_hd,
        HomeRequest::Report {
            event: "status".into(),
            units: vec![ReportedUnit {
                rel: "work".into(),
                decision: crate::decide::Decision::Push,
            }],
            summary: "push".into(),
        },
    ));
    // spruce last talked to home nine days ago.
    let old = fleet::Contact {
        version: crate::record::RECORD_VERSION,
        rel: "spruce".into(),
        last_contact: crate::now_secs() - 9 * 86_400,
        report: None,
    };
    fleet::contacts(&bed.home_dirs.state()).put(&old).unwrap();
    let HomeReply::Fleet(s) = ok(call(&home, "leaf", &bed.leaf_hd, HomeRequest::FleetState)) else {
        panic!()
    };
    let row = |n: &str| s.hosts.iter().find(|h| h.name == n).unwrap().clone();
    assert!(!row("leaf").stale);
    assert_eq!(
        row("leaf").report.unwrap().units[0].decision,
        crate::decide::Decision::Push
    );
    assert!(row("spruce").stale);
    let journal = home
        .log(&Filter {
            host: Some("leaf".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(journal[0].event, "status");
    let _ = PathBuf::new();
}
