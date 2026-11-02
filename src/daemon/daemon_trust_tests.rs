//! The trust model over real mTLS: what the bootstrap bundle may do, and
//! what a host's own certificate may do.

use super::test_certs::{dial, leaf_identity};
use super::*;
use crate::testutil::Bed;
use wire::{HomeReply, HomeRequest};

fn every_other_request() -> Vec<HomeRequest> {
    vec![
        HomeRequest::Unsubscribe {
            paths: vec!["~/work".into()],
        },
        HomeRequest::Subscriptions,
        HomeRequest::Acquire { no_wait: true },
        HomeRequest::Heartbeat { lease: 1 },
        HomeRequest::Release { lease: 1 },
        HomeRequest::State {
            leaf_repos: vec![],
            case_check: false,
            baselines: vec![],
        },
        HomeRequest::Report {
            event: "dry-run".into(),
            units: vec![],
            summary: String::new(),
        },
        HomeRequest::FleetState,
    ]
}

#[test]
fn the_bootstrap_bundle_may_only_ask_to_be_enrolled() {
    let bed = Bed::enrolled();
    let home = Daemon::start(bed.home_dirs.clone()).unwrap();
    let fresh = dial(&home, &bed.home_dirs.credentials(), "fresh", &bed.leaf_hd);

    let Ok(HomeReply::Subscribed(out)) = fresh.call(HomeRequest::Subscribe {
        paths: vec!["~/work".into()],
    }) else {
        panic!()
    };
    assert!(!out.enrolled, "pending");

    for request in every_other_request() {
        let err = fresh.refused(request);
        assert!(err.contains("bootstrap bundle"), "{err}");
    }
    // Not even as an enrolled host, and not even to subscribe.
    let as_leaf = fresh.as_host("leaf");
    let before = std::fs::read_to_string(bed.home_dirs.fleet()).unwrap();
    let err = as_leaf.refused(HomeRequest::Subscribe {
        paths: vec!["~/work/x".into()],
    });
    assert!(err.contains("leaf is enrolled"), "{err}");
    for request in every_other_request() {
        let err = as_leaf.refused(request);
        assert!(err.contains("bootstrap bundle"), "{err}");
    }
    assert_eq!(
        std::fs::read_to_string(bed.home_dirs.fleet()).unwrap(),
        before,
        "nothing written for the enrolled host"
    );
    home.stop();
}

#[test]
fn a_client_certificate_that_is_not_the_bootstrap_is_not_gated_by_it() {
    let bed = Bed::enrolled();
    let home = Daemon::start(bed.home_dirs.clone()).unwrap();
    let id = leaf_identity(&bed, "leaf");
    let leaf = dial(&home, &id, "leaf", &bed.leaf_hd);
    assert!(matches!(
        leaf.call(HomeRequest::Subscriptions),
        Ok(HomeReply::Subscriptions(_))
    ));
    home.stop();
}
