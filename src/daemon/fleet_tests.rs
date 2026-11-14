use super::*;
use crate::daemon::queue::EntryKind;
use crate::decide::Decision;

const FLEET: &str = "[daemon]\nname=\"tc\"\nhome_dir=\"/h\"\nstale_after_hours=1\n\
    [roots.work]\npath=\"/h/work\"\n\
    [hosts.fresh]\nhome_dir=\"/u\"\n[hosts.stale]\nhome_dir=\"/u\"\nallow=[\"work/x\"]\n\
    [hosts.never]\nhome_dir=\"/u\"\n\
    [subscriptions.stale]\npaths=[\"work/x\"]\n\
    [pending.newbie]\nhome_dir=\"/u\"\npaths=[\"work\"]\nrequested_at=5\n";

#[test]
fn one_row_per_host_with_staleness_reports_and_queue() {
    let tmp = tempfile::tempdir().unwrap();
    let fleet = Fleet::parse(FLEET, Path::new("fleet.toml")).unwrap();
    let store = contacts(tmp.path());
    let now = crate::now_secs();
    store
        .put(&Contact {
            version: RECORD_VERSION,
            rel: "fresh".into(),
            last_contact: now - 60,
            report: None,
            last_sync: None,
            markers: vec![],
        })
        .unwrap();
    let report = LastReport {
        at: now - 9 * 86_400,
        event: "dry-run".into(),
        units: vec![ReportedUnit {
            rel: "work/x".into(),
            decision: Decision::Push,
        }],
    };
    store
        .put(&Contact {
            version: RECORD_VERSION,
            rel: "stale".into(),
            last_contact: now - 9 * 86_400,
            report: Some(report.clone()),
            last_sync: None,
            markers: vec![],
        })
        .unwrap();
    let q = vec![QueueView {
        id: 1,
        host: "stale".into(),
        kind: EntryKind::DryRun,
        paths: vec![],
        enqueued_at: now,
        started_at: Some(now),
    }];
    let s = build(&fleet, &store, q, vec![], now).unwrap();
    assert_eq!(s.home, "tc");
    let names: Vec<_> = s.hosts.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(names, ["fresh", "never", "stale"]);
    let row = |n: &str| s.hosts.iter().find(|h| h.name == n).unwrap();
    assert!(!row("fresh").stale);
    assert!(row("stale").stale, "nine days without contact");
    assert!(row("never").stale && row("never").last_contact.is_none());
    assert_eq!(row("stale").report, Some(report));
    assert_eq!(row("stale").queue_position, Some(0));
    assert_eq!(row("stale").cap, Some(vec!["work/x".to_string()]));
    assert_eq!(s.pending.len(), 1);
    assert_eq!(s.pending[0].host, "newbie");
}

#[test]
fn touch_keeps_the_last_report() {
    let tmp = tempfile::tempdir().unwrap();
    let store = contacts(tmp.path());
    let r = LastReport {
        at: 1,
        event: "status".into(),
        units: vec![],
    };
    touch(&store, "h", Some(r.clone())).unwrap();
    touch(&store, "h", None).unwrap();
    let c: Contact = store.get("h").unwrap().unwrap();
    assert_eq!(c.report, Some(r));
    assert!(c.last_contact > 0);
}

#[test]
fn a_report_keeps_the_last_sync_until_the_next_sync() {
    let tmp = tempfile::tempdir().unwrap();
    let store = contacts(tmp.path());
    let r = |event: &str| LastReport {
        at: 1,
        event: event.into(),
        units: vec![],
    };
    let m = Marker {
        version: RECORD_VERSION,
        rel: "work/x".into(),
        direction: crate::marker::Direction::Pull,
        host: "h".into(),
        source_state: crate::state::UnitState::Empty,
        started_at: 5,
        push: None,
        backup: None,
    };
    let sync = LastSync {
        at: 9,
        ok: false,
        summary: "interrupted".into(),
    };
    reported(&store, "h", r("sync"), vec![m.clone()], Some(sync.clone())).unwrap();
    reported(&store, "h", r("status"), vec![m.clone()], None).unwrap();
    touch(&store, "h", None).unwrap();
    let c: Contact = store.get("h").unwrap().unwrap();
    assert_eq!(
        c.last_sync,
        Some(sync),
        "a status after it does not erase it"
    );
    assert_eq!(c.markers, vec![m]);
    assert_eq!(c.report.unwrap().event, "status");
    reported(&store, "h", r("status"), vec![], None).unwrap();
    let c: Contact = store.get("h").unwrap().unwrap();
    assert!(c.markers.is_empty(), "markers are as of the last report");
}
