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
        elapsed_ms: 842_000,
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
        push: None,
        backup: None,
        elapsed_ms: 1_400,
    }
}

#[test]
fn each_outcome_says_what_was_written_and_what_comes_next() {
    let r = report(Outcome::Done, vec![unit("work/a", "pull", false)]);
    // The units went by as they ran: only --verbose lists them again,
    // each with how long it took.
    let s = synced(&r, true, plain());
    assert!(
        s.contains("pull            work/a  3 files changed, 1 deleted, 1.4s"),
        "{s}"
    );
    assert!(s.ends_with("done: 1 unit applied in 14m 02s\n"), "{s}");
    assert_eq!(
        synced(&r, false, plain()),
        "done: 1 unit applied in 14m 02s\n"
    );
    assert_eq!(
        synced(&report(Outcome::Done, vec![]), false, plain()),
        "nothing to apply\n"
    );

    let s = synced(
        &report(
            Outcome::Blocked {
                why: vec!["work/r: conflict (x)".into()],
            },
            vec![],
        ),
        false,
        plain(),
    );
    assert!(
        s.contains("blocked; nothing written:\n  work/r: conflict (x)"),
        "{s}"
    );
    assert!(synced(&report(Outcome::Changed, vec![]), false, plain()).contains("nothing written"));

    let s = synced(
        &report(
            Outcome::Interrupted {
                rel: "work/b".into(),
                why: "the other side closed the data connection".into(),
                journaled: true,
            },
            vec![unit("work/a", "push", true)],
        ),
        true,
        plain(),
    );
    assert!(s.contains(", resumed"), "{s}");
    assert!(
        s.contains("interrupted at work/b: the other side closed the data connection"),
        "{s}"
    );
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
        false,
        plain(),
    );
    assert!(s.contains("failed at work/b: home unreachable"), "{s}");
    let red = synced(
        &report(Outcome::Changed, vec![]),
        false,
        Style { color: true },
    );
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
            push: None,
            backup: None,
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

#[test]
fn pushes_list_their_state_and_backups() {
    use crate::daemon::home::{Push, Run};
    assert_eq!(pushes(&[], 0), "no pushes recorded\n");
    let p = |id: &str, finished: Option<u64>, rolled: Option<u64>| Push {
        version: 1,
        id: id.into(),
        host: "leaf".into(),
        unit: "work".into(),
        is_file: false,
        started_at: 100,
        runs: vec![Run {
            started_at: 100,
            backed_up: vec!["a".into(), "b".into()],
            dirs: vec![],
            absent: vec![],
            changes: vec![],
            reported: true,
        }],
        finished_at: finished,
        rolled_back_at: rolled,
        rollback_settled_at: None,
    };
    let s = pushes(
        &[
            p("leaf/20261101T000000Z", Some(1), None),
            p("leaf/20261101T000001Z", None, None),
            p("leaf/20261101T000002Z", Some(1), Some(2)),
        ],
        3700,
    );
    assert!(
        s.contains("leaf/20261101T000000Z  work  1h ago  finished, 1 run(s), 2 files backed up"),
        "{s}"
    );
    assert!(
        s.contains("leaf/20261101T000001Z  work  1h ago  interrupted"),
        "{s}"
    );
    assert!(
        s.contains("leaf/20261101T000002Z  work  1h ago  rolled back"),
        "{s}"
    );
}

#[path = "render_progress_tests.rs"]
mod progress_view;

/// A resolved pull's backup on the leaf is said whether or not the units
/// are listed: nothing else tells the person where it went (rev 22).
#[test]
fn a_resolved_pulls_backup_is_always_said() {
    let mut a = unit("work/r", "pull (resolved)", false);
    a.backup = Some("/Users/b/.sovran/replicant/backup/resolved/t/work/r".into());
    let r = report(Outcome::Done, vec![a, unit("work/s", "pull", false)]);
    let line = "work/r: what this pull replaced here is kept in \
                /Users/b/.sovran/replicant/backup/resolved/t/work/r\n";
    for list in [false, true] {
        let s = synced(&r, list, Style { color: false });
        assert_eq!(s.matches(line).count(), 1, "{s}");
        assert!(!s.contains("work/s: what"), "{s}");
    }
}
