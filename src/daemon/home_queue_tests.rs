//! The queue and fleet_state through home's handler.

use super::*;

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
        HomeRequest::Acquire {
            no_wait: true,
            apply: false,
        },
    )) else {
        panic!()
    };
    assert_eq!(lease_secs, 1);
    let busy = call(
        &home,
        "leaf",
        &bed.leaf_hd,
        HomeRequest::Acquire {
            no_wait: true,
            apply: false,
        },
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
}
