use super::*;
use crate::daemon::journal::Filter;
use crate::testutil::{self, Bed};

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
            baselines: vec![],
        },
    );
    assert!(st.result.unwrap_err().contains("pending approval"));
    let acq = call(
        &home,
        "newbox",
        &bed.leaf_hd,
        HomeRequest::Acquire {
            no_wait: true,
            apply: false,
        },
    );
    assert!(
        acq.result.is_err(),
        "a pending host cannot take a queue entry"
    );

    assert_eq!(home.pending().unwrap()[0].host, "newbox");
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
    assert_eq!(
        log[1].detail,
        format!("allow:work/tundra cert:{}", approved.cert),
        "approve journals the pin it wrote"
    );
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
            baselines: vec![],
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
    // Home's policy, not a broken config: orphaned, not refused (F5).
    assert!(v.refused.is_empty(), "{:?}", v.refused);
    assert_eq!(v.orphaned.len(), 1);
    assert_eq!(v.orphaned[0].unit, "work/b");

    let HomeReply::State(s) = ok(call(
        &home,
        "leaf",
        &bed.leaf_hd,
        HomeRequest::State {
            leaf_repos: vec!["work/b/r".into(), "work/../etc".into(), "other/x".into()],
            case_check: false,
            baselines: vec![],
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
        push: None,
        backup: None,
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
            baselines: vec![],
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

#[path = "home_queue_tests.rs"]
mod more;

#[path = "home_apply_tests.rs"]
mod apply;
