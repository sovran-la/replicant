//! `sync`'s report and `status`'s open markers, as text.

use super::*;
use crate::marker::{Direction, Marker};
use crate::plan::Plan;
use crate::plan::apply::AppliedUnit;
use crate::record::RECORD_VERSION;
use crate::state::UnitState;
use crate::units::UnitKind;

fn plain() -> Style {
    Style { color: false }
}

fn report(outcome: Outcome, applied: Vec<AppliedUnit>) -> SyncReport {
    SyncReport {
        plan: Plan {
            me: "leaf".into(),
            home: "home".into(),
            dry_run: true,
            units: vec![],
            refused: vec![],
            orphaned: vec![],
            timings: Default::default(),
        },
        outcome,
        applied,
    }
}

fn unit(rel: &str, action: &str, resumed: bool) -> AppliedUnit {
    AppliedUnit {
        rel: rel.into(),
        kind: UnitKind::Repo,
        action: action.into(),
        resumed,
        changes: vec![],
        files_changed: 3,
        deleted: 1,
    }
}

#[test]
fn each_outcome_says_what_was_written_and_what_comes_next() {
    let r = report(Outcome::Done, vec![unit("work/a", "pull", false)]);
    let s = synced(&r, plain());
    assert!(
        s.contains("pull            work/a  3 files changed, 1 deleted"),
        "{s}"
    );
    assert!(s.ends_with("done: 1 unit applied\n"), "{s}");
    assert_eq!(
        synced(&report(Outcome::Done, vec![]), plain()),
        "nothing to apply\n"
    );

    let s = synced(
        &report(
            Outcome::Blocked {
                why: vec!["work/r: conflict (x)".into()],
            },
            vec![],
        ),
        plain(),
    );
    assert!(
        s.contains("blocked; nothing written:\n  work/r: conflict (x)"),
        "{s}"
    );
    assert!(synced(&report(Outcome::Changed, vec![]), plain()).contains("nothing written"));

    let s = synced(
        &report(
            Outcome::Interrupted {
                rel: "work/b".into(),
                why: "rsync exit 23".into(),
                journaled: true,
            },
            vec![unit("work/a", "push", true)],
        ),
        plain(),
    );
    assert!(s.contains(", resumed"), "{s}");
    assert!(s.contains("interrupted at work/b: rsync exit 23"), "{s}");
    assert!(
        s.contains("marker stays") && s.contains("nothing after it was touched"),
        "{s}"
    );
    let s = synced(
        &report(
            Outcome::Failed {
                rel: "work/b".into(),
                why: "home unreachable".into(),
            },
            vec![],
        ),
        plain(),
    );
    assert!(s.contains("failed at work/b: home unreachable"), "{s}");
    let red = synced(&report(Outcome::Changed, vec![]), Style { color: true });
    assert!(red.contains("\x1b[31m"), "red when coloured");
}

fn open(rel: &str, host: &str, on: &str, age: u64, alert: bool) -> OpenMarker {
    OpenMarker {
        marker: Marker {
            version: RECORD_VERSION,
            rel: rel.into(),
            direction: Direction::Pull,
            host: host.into(),
            source_state: UnitState::Empty,
            started_at: 1,
        },
        on: on.into(),
        age_secs: age,
        alert,
    }
}

#[test]
fn open_markers_show_age_owner_and_what_happens_next() {
    assert_eq!(open_markers(&[], "leaf", "home", plain()), "");
    let s = open_markers(
        &[
            open("work/a", "leaf", "leaf", 7200, true),
            open("work/b", "other", "home", 30, false),
        ],
        "leaf",
        "home",
        plain(),
    );
    assert!(s.starts_with("open markers:\n"), "{s}");
    assert!(
        s.contains(
            "work/a  pull by leaf, marked on leaf, started 2h ago; the next `replicant sync` \
             resumes it (interrupted)"
        ),
        "{s}"
    );
    assert!(
        s.contains("work/b  pull by other, marked on home, started 30s ago; quarantined"),
        "{s}"
    );
    assert!(s.contains("(may still be running)"), "{s}");
    let c = open_markers(
        &[open("w", "leaf", "leaf", 9, true)],
        "leaf",
        "home",
        Style { color: true },
    );
    assert!(c.contains("\x1b[31m"), "an interrupted one is red");
    let c = open_markers(
        &[open("w", "leaf", "leaf", 9, false)],
        "leaf",
        "home",
        Style { color: true },
    );
    assert!(c.contains("\x1b[2m"), "a young one is dim");
}
