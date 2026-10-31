use super::*;
use crate::daemon::fleet::{FleetState, HostRow, LastReport, PendingView};
use crate::daemon::journal::Line;
use crate::daemon::wire::ReportedUnit;
use crate::guard::{Guard, Refusal};
use crate::plan::{DiffSummary, Timings};
use crate::state::UnitState;
use std::path::PathBuf;

const PLAIN: Style = Style { color: false };
const COLOR: Style = Style { color: true };

fn unit(rel: &str, decision: Decision) -> PlannedUnit {
    PlannedUnit {
        rel: rel.into(),
        kind: UnitKind::Repo,
        decision,
        leaf: UnitState::Empty,
        home: UnitState::Present("abcdef0123456789".into()),
        leaf_head: None,
        home_head: Some("ref: refs/heads/main".into()),
        leaf_path: PathBuf::from("/l"),
        home_path: PathBuf::from("/h"),
        diff: None,
        refusals: vec![],
    }
}

fn a_plan() -> Plan {
    let mut pull = unit("work/a", Decision::Pull);
    pull.diff = Some(DiffSummary {
        files_changed: 20,
        deleted: 1,
        changes: (0..20).map(|i| format!("+ f{i}")).collect(),
        notes: vec![],
    });
    let mut locked = unit("work/b", Decision::Pull);
    locked.refusals = vec![Refusal {
        guard: Guard::SourceMidWrite,
        unit: "work/b".into(),
        detail: vec![".git/index.lock".into()],
    }];
    locked.diff = Some(DiffSummary {
        files_changed: 1,
        deleted: 0,
        changes: vec!["~ x".into()],
        notes: vec![],
    });
    Plan {
        me: "birchbook".into(),
        home: "thistledown".into(),
        dry_run: true,
        units: vec![
            pull,
            locked,
            unit(
                "work/c",
                Decision::Conflict {
                    why: "no baseline, both populated".into(),
                },
            ),
            unit(
                "work/d",
                Decision::Nothing {
                    why: "identical".into(),
                },
            ),
        ],
        refused: vec![],
        timings: Timings {
            total_ms: 1234,
            ..Default::default()
        },
    }
}

#[test]
fn plan_prints_the_decision_line_first() {
    let s = plan(&a_plan(), false, PLAIN);
    let first = s.lines().next().unwrap();
    assert_eq!(first, "conflict: 1 units");
    assert!(
        s.contains("pull ← thistledown: 2 repos, 21 files changed, 1 deleted"),
        "{s}"
    );
    assert!(
        s.contains("REFUSED guardrail 3 (source mid-write): work/b"),
        "{s}"
    );
    assert!(s.contains(".git/index.lock"));
    assert!(
        s.contains("… 8 more (--verbose)"),
        "diff preview is capped: {s}"
    );
    assert!(
        s.contains("1 units in sync"),
        "nothing rows are folded: {s}"
    );
    assert!(!s.contains("work/d"));
    assert!(s.contains("timing: total 1234ms"));
    assert!(s.contains("dry run: nothing was written"));
    assert!(s.contains("home main abcdef012345"), "{s}");
}

#[test]
fn verbose_lists_everything() {
    let s = plan(&a_plan(), true, PLAIN);
    assert!(s.contains("+ f19") && s.contains("work/d"));
    assert!(!s.contains("more (--verbose)"));
}

#[test]
fn conflicts_and_refusals_are_red_with_color() {
    let s = plan(&a_plan(), false, COLOR);
    assert!(
        s.contains("\x1b[31mconflict (no baseline, both populated)\x1b[0m"),
        "{s}"
    );
    assert!(s.contains("\x1b[31mREFUSED"));
    assert!(!plan(&a_plan(), false, PLAIN).contains('\x1b'));
}

fn fleet_state() -> FleetState {
    let row = |name: &str, last: Option<u64>, stale: bool, report: Option<LastReport>| HostRow {
        name: name.into(),
        home_dir: "/Users/morgan".into(),
        cap: None,
        paths: vec!["work".into()],
        except: vec![],
        last_contact: last,
        stale,
        report,
        queue_position: None,
    };
    let now = 10 * 86_400;
    FleetState {
        home: "thistledown".into(),
        now,
        stale_after_secs: 3 * 86_400,
        hosts: vec![
            row(
                "birchbook",
                Some(now - 3600),
                false,
                Some(LastReport {
                    at: now - 3600,
                    event: "status".into(),
                    units: vec![
                        ReportedUnit {
                            rel: "work/a".into(),
                            decision: Decision::Pull,
                        },
                        ReportedUnit {
                            rel: "work/b".into(),
                            decision: Decision::Pull,
                        },
                        ReportedUnit {
                            rel: "work/c".into(),
                            decision: Decision::Nothing { why: "x".into() },
                        },
                    ],
                }),
            ),
            row("spruce", Some(now - 9 * 86_400), true, None),
        ],
        pending: vec![PendingView {
            host: "newbox".into(),
            home_dir: "/home/morgan".into(),
            paths: vec!["work".into()],
            requested_at: now,
        }],
        queue: vec![],
        markers: vec![],
    }
}

#[test]
fn fleet_one_row_per_host_with_staleness() {
    let s = fleet(&fleet_state(), PLAIN);
    assert!(
        s.starts_with("home thistledown — 2 hosts, 1 pending, 0 in queue"),
        "{s}"
    );
    let bb = s.lines().find(|l| l.contains("birchbook")).unwrap();
    assert!(
        bb.contains("1h ago") && bb.contains("2 pull") && bb.contains("(as of status 1h ago)"),
        "{bb}"
    );
    let sp = s.lines().find(|l| l.contains("spruce")).unwrap();
    assert!(
        sp.contains("STALE 9d ago") && sp.contains("no report yet"),
        "{sp}"
    );
    assert!(s.contains("newbox") && s.contains("replicant approve"));
    assert!(fleet(&fleet_state(), COLOR).contains("\x1b[31mSTALE 9d ago"));
}

#[test]
fn subscribe_outcomes_explain_what_happens_next() {
    let pending = SubscribeOutcome {
        enrolled: false,
        home: "thistledown".into(),
        requested: vec!["work/tundra".into()],
        paths: vec!["work/tundra".into()],
        absorbed: vec![],
    };
    let s = subscribed(&pending);
    assert!(s.contains("On thistledown run `replicant approve`"), "{s}");
    let ok = SubscribeOutcome {
        enrolled: true,
        absorbed: vec!["work/x".into()],
        ..pending
    };
    let s = subscribed(&ok);
    assert!(
        s.contains("subscribed on thistledown") && s.contains("absorbed"),
        "{s}"
    );
}

#[test]
fn log_lines_are_aligned_and_utc() {
    let s = log(&[Line {
        ts: 0,
        event: "approve".into(),
        host: "spruce".into(),
        path: "-".into(),
        detail: "allow:(none)".into(),
    }]);
    assert!(s.starts_with("1970-01-01T00:00:00Z  approve "), "{s}");
    assert!(s.trim_end().ends_with("allow:(none)"));
}
