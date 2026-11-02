//! Identity over real mTLS: home derives the host from the peer's pinned
//! certificate, never from the name in the request (review F12/Q1).

use super::test_certs::{RawLeaf, dial, leaf_identity};
use super::*;
use crate::config::Fleet;
use crate::daemon::journal::{Filter, Line};
use crate::testutil::{self, Bed};
use wire::{HomeReply, HomeRequest};

/// `leaf` subscribed to work/a, `other` to work/b, both enrolled.
fn two_hosts() -> Bed {
    let b = Bed::new("");
    let hd = b.leaf_hd.display().to_string();
    let extra = format!(
        "[hosts.leaf]\nhome_dir = \"{hd}\"\n\n[hosts.other]\nhome_dir = \"{hd}\"\n\n\
         [subscriptions.leaf]\npaths = [\"work/a\"]\n\n[subscriptions.other]\npaths = [\"work/b\"]\n"
    );
    let mut text = std::fs::read_to_string(b.home_dirs.fleet()).unwrap();
    text.push_str(&extra);
    testutil::write(&b.home_dirs.fleet(), &text);
    b
}

fn journal(bed: &Bed) -> Vec<Line> {
    match local::call(
        &bed.home_dirs,
        LocalRequest::Log {
            filter: Filter::default(),
        },
    )
    .unwrap()
    {
        LocalReply::Log { lines } => lines,
        other => panic!("{other:?}"),
    }
}

fn subscription(bed: &Bed, host: &str) -> crate::config::Subscription {
    Fleet::load(&bed.home_dirs.fleet())
        .unwrap()
        .subscription(host)
}

fn as_itself(leaf: &RawLeaf) -> Vec<String> {
    match leaf.call(HomeRequest::Subscriptions).unwrap() {
        HomeReply::Subscriptions(v) => v.specs.into_iter().map(|s| s.rel).collect(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_pinned_certificate_is_its_host_and_only_its_host() {
    let bed = two_hosts();
    let home = Daemon::start(bed.home_dirs.clone()).unwrap();
    let a = dial(&home, &leaf_identity(&bed, "leaf"), "leaf", &bed.leaf_hd);
    let b = dial(&home, &leaf_identity(&bed, "other"), "other", &bed.leaf_hd);
    assert_eq!(as_itself(&a), vec!["work/a"]);
    assert_eq!(as_itself(&b), vec!["work/b"]);

    // F12: leaf's certificate naming `other` is refused for everything,
    // whatever it asks, and changes nothing of other's.
    let a_as_b = a.as_host("other");
    let before = std::fs::read_to_string(bed.home_dirs.fleet()).unwrap();
    for request in [
        HomeRequest::Unsubscribe {
            paths: vec!["~/work/b".into()],
        },
        HomeRequest::Subscribe {
            paths: vec!["~/work/c".into()],
        },
        HomeRequest::Subscriptions,
        HomeRequest::Acquire { no_wait: true },
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
        HomeRequest::CollectCert,
    ] {
        let err = a_as_b.refused(request);
        assert!(err.contains("this certificate is leaf's"), "{err}");
    }
    assert_eq!(
        std::fs::read_to_string(bed.home_dirs.fleet()).unwrap(),
        before,
        "nothing written"
    );
    let refused: Vec<Line> = journal(&bed)
        .into_iter()
        .filter(|l| l.event == "refused")
        .collect();
    assert_eq!(refused.len(), 8, "every mismatch journaled: {refused:?}");
    assert!(
        refused
            .iter()
            .all(|l| l.host == "leaf"
                && l.detail == "certificate is leaf's; the request named other")
    );

    // As itself, leaf cannot reach other's paths: unsubscribing work/b is
    // about leaf's subscription, which does not hold it.
    let err = a.refused(HomeRequest::Unsubscribe {
        paths: vec!["~/work/b".into()],
    });
    assert!(err.contains("leaf is not subscribed to work/b"), "{err}");
    assert_eq!(subscription(&bed, "other").paths, vec!["work/b"]);
    home.stop();
}

#[test]
fn a_lease_is_heartbeated_and_released_only_by_its_holder() {
    let bed = two_hosts();
    let home = Daemon::start(bed.home_dirs.clone()).unwrap();
    let a = dial(&home, &leaf_identity(&bed, "leaf"), "leaf", &bed.leaf_hd);
    let b = dial(&home, &leaf_identity(&bed, "other"), "other", &bed.leaf_hd);
    let Ok(HomeReply::Lease { lease, .. }) = b.call(HomeRequest::Acquire { no_wait: true }) else {
        panic!()
    };
    for request in [
        HomeRequest::Heartbeat { lease },
        HomeRequest::Release { lease },
    ] {
        let err = a.refused(request);
        assert!(err.contains(&format!("lease {lease} is other's")), "{err}");
    }
    b.call(HomeRequest::Heartbeat { lease }).unwrap();
    b.call(HomeRequest::Release { lease }).unwrap();
    home.stop();
}

#[test]
fn an_unpinned_certificate_is_refused_and_journaled() {
    let bed = two_hosts();
    let home = Daemon::start(bed.home_dirs.clone()).unwrap();
    // Signed by home's CA, but no [hosts.*] entry pins it.
    let ghost = dial(&home, &leaf_identity(&bed, "ghost"), "ghost", &bed.leaf_hd);
    let err = ghost.refused(HomeRequest::Subscribe {
        paths: vec!["~/work".into()],
    });
    assert!(err.contains(super::home::UNKNOWN_CERT), "{err}");
    assert!(
        Fleet::load(&bed.home_dirs.fleet())
            .unwrap()
            .pending
            .is_empty(),
        "not even a pending entry"
    );
    let line = journal(&bed)
        .into_iter()
        .find(|l| l.event == "refused")
        .unwrap();
    assert_eq!(line.host, "ghost");
    assert!(
        line.detail.starts_with("unknown certificate sha256:"),
        "{}",
        line.detail
    );
    home.stop();
}

/// F11 for identity: a pin removed from fleet.toml by hand refuses the
/// certificate on its next request, with no restart.
#[test]
fn a_pin_removed_by_hand_is_refused_on_the_next_request() {
    let bed = two_hosts();
    let home = Daemon::start(bed.home_dirs.clone()).unwrap();
    let a = dial(&home, &leaf_identity(&bed, "leaf"), "leaf", &bed.leaf_hd);
    assert_eq!(as_itself(&a), vec!["work/a"]);
    let pin = Fleet::load(&bed.home_dirs.fleet()).unwrap().hosts["leaf"]
        .cert
        .clone()
        .unwrap();
    testutil::hand_edit(&bed.home_dirs.fleet(), &format!("cert = \"{pin}\"\n"), "");
    let err = a.refused(HomeRequest::Subscriptions);
    assert!(err.contains(super::home::UNKNOWN_CERT), "{err}");
    home.stop();
}

/// Review P5 over the wire: a broken fleet.toml refuses whatever reads
/// policy, but a granted lease is still heartbeated and released, the
/// holder identified by the pins of the last good fleet.toml.
#[test]
fn a_broken_fleet_toml_keeps_granted_leases_for_their_holders() {
    let bed = two_hosts();
    let home = Daemon::start(bed.home_dirs.clone()).unwrap();
    let a = dial(&home, &leaf_identity(&bed, "leaf"), "leaf", &bed.leaf_hd);
    let ghost = dial(&home, &leaf_identity(&bed, "ghost"), "ghost", &bed.leaf_hd);
    let Ok(HomeReply::Lease { lease, .. }) = a.call(HomeRequest::Acquire { no_wait: true }) else {
        panic!()
    };
    testutil::hand_edit(&bed.home_dirs.fleet(), "[hosts.leaf]\n", "[hosts.leaf\n");
    a.call(HomeRequest::Heartbeat { lease }).unwrap();
    let err = a.refused(HomeRequest::Subscriptions);
    assert!(err.contains("fleet.toml"), "fails closed: {err}");
    let err = ghost.refused(HomeRequest::Heartbeat { lease });
    assert!(err.contains(super::home::UNKNOWN_CERT), "{err}");
    a.call(HomeRequest::Release { lease }).unwrap();
    home.stop();
}
