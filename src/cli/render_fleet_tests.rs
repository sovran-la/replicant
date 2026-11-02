//! The fleet view's apply lines: each host's last sync, its open markers,
//! and home's quarantines with their age and how to undo them.

use super::*;
use crate::daemon::fleet::{HostRow, LastSync};
use crate::marker::{Direction, Marker};
use crate::record::RECORD_VERSION;
use crate::state::UnitState;

fn marker(rel: &str, host: &str, at: u64, push: Option<&str>) -> Marker {
    Marker {
        version: RECORD_VERSION,
        rel: rel.into(),
        direction: Direction::Push,
        host: host.into(),
        source_state: UnitState::Empty,
        started_at: at,
        push: push.map(String::from),
    }
}

fn row(name: &str, last_sync: Option<LastSync>, markers: Vec<Marker>) -> HostRow {
    HostRow {
        name: name.into(),
        home_dir: "/u".into(),
        cap: None,
        paths: vec!["work".into()],
        except: vec![],
        last_contact: Some(1000),
        stale: false,
        report: None,
        queue_position: None,
        last_sync,
        markers,
    }
}

#[test]
fn last_sync_open_markers_and_quarantines() {
    let f = FleetState {
        home: "tc".into(),
        now: 10_000,
        stale_after_secs: 86_400,
        hosts: vec![
            row(
                "good",
                Some(LastSync {
                    at: 10_000 - 7200,
                    ok: true,
                    summary: "3 units, 9 files, 0 del; ok".into(),
                }),
                vec![],
            ),
            row(
                "bad",
                Some(LastSync {
                    at: 10_000 - 60,
                    ok: false,
                    summary: "interrupted at work/b".into(),
                }),
                vec![marker("work/b", "bad", 10_000 - 60, None)],
            ),
            row("new", None, vec![]),
        ],
        pending: vec![],
        queue: vec![],
        markers: vec![marker(
            "work/c",
            "bad",
            10_000 - 3600,
            Some("bad/20261101T000000Z"),
        )],
    };
    let s = fleet(&f, Style { color: false });
    assert!(
        s.contains("last sync: 2h ago, 3 units, 9 files, 0 del; ok"),
        "{s}"
    );
    assert!(
        s.contains("last sync: 1m ago, interrupted at work/b"),
        "{s}"
    );
    assert!(
        s.contains("open marker: push work/b on bad, started 1m ago"),
        "{s}"
    );
    assert!(s.contains("last sync: never"), "{s}");
    assert!(
        s.contains("quarantined: work/c (dead push by bad, since ")
            && s.contains(", 1h ago; `replicant rollback bad/20261101T000000Z` undoes it)"),
        "{s}"
    );
    let c = fleet(&f, Style { color: true });
    assert!(
        c.contains("\x1b[31mlast sync: 1m ago, interrupted"),
        "a failed sync is red"
    );
    assert!(
        !c.contains("\x1b[31mlast sync: 2h ago"),
        "a good one is not"
    );
}
