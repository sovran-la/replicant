//! The trust model over real mTLS: what the bootstrap bundle may do, and
//! what a host's own certificate may do.

use super::test_certs::{dial, leaf_identity, pair};
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
fn a_pinned_certificate_is_not_gated_like_the_bootstrap() {
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

#[test]
fn collect_says_unknown_then_pending_then_hands_over_once() {
    let bed = Bed::new("");
    let home = Daemon::start(bed.home_dirs.clone()).unwrap();
    let fresh = dial(&home, &bed.home_dirs.credentials(), "fresh", &bed.leaf_hd);
    let collect = || fresh.call(HomeRequest::CollectCert);

    assert!(matches!(
        collect(),
        Ok(HomeReply::Collected(wire::Collected::Unknown { ref home })) if home == "home"
    ));
    fresh
        .call(HomeRequest::Subscribe {
            paths: vec!["~/work".into()],
        })
        .unwrap();
    let Ok(HomeReply::Collected(wire::Collected::Pending { paths, .. })) = collect() else {
        panic!()
    };
    assert_eq!(paths, vec!["work"]);

    let LocalReply::Approved(a) = local::call(
        &bed.home_dirs,
        LocalRequest::Approve {
            host: "fresh".into(),
            allow: vec![],
        },
    )
    .unwrap() else {
        panic!()
    };
    let Ok(HomeReply::Collected(wire::Collected::Issued { bundle })) = collect() else {
        panic!()
    };
    let id = tls::parse(&bundle, std::path::Path::new("x")).unwrap();
    assert_eq!(id.fingerprint(), a.cert);

    let err = fresh.refused(HomeRequest::CollectCert);
    assert!(err.contains("no certificate is waiting"), "one-shot: {err}");
    assert!(!bed.home_dirs.issued().join("fresh.pem").exists());
    home.stop();
}

#[test]
fn only_the_bootstrap_collects() {
    let bed = Bed::enrolled();
    let home = Daemon::start(bed.home_dirs.clone()).unwrap();
    let leaf = dial(&home, &leaf_identity(&bed, "leaf"), "leaf", &bed.leaf_hd);
    let err = leaf.refused(HomeRequest::CollectCert);
    assert!(err.contains("already has its own certificate"), "{err}");
    home.stop();
}

/// `approve <host>` on an enrolled host replaces its pin; the leaf's next
/// command finds its certificate unknown, collects the new one over the
/// bootstrap, and goes on as if nothing happened.
#[test]
fn a_reissued_certificate_is_collected_by_the_leafs_next_command() {
    let bed = Bed::enrolled();
    let (home, leaf) = pair(&bed);
    let own = || tls::Identity::load(&bed.leaf_dirs.host_identity()).unwrap();
    let before = own().fingerprint();
    assert!(matches!(
        local::call(&bed.leaf_dirs, LocalRequest::Status),
        Ok(LocalReply::LeafStatus(_))
    ));
    let LocalReply::Reissued(r) = local::call(
        &bed.home_dirs,
        LocalRequest::Reissue {
            host: "leaf".into(),
        },
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(r.replaced.as_deref(), Some(before.as_str()));
    let Ok(LocalReply::LeafStatus(st)) = local::call(&bed.leaf_dirs, LocalRequest::Status) else {
        panic!()
    };
    assert_eq!(st.enrollment, wire::Enrollment::Enrolled);
    assert_eq!(own().fingerprint(), r.cert, "host.pem replaced");
    assert!(!bed.home_dirs.issued().join("leaf.pem").exists());
    leaf.stop();
    home.stop();
}
