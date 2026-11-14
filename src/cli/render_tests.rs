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
        leaf_files: 0,
        home_files: 0,
        resolvable: false,
        leaf_dirty: 0,
        home_dirty: 0,
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
        orphaned: vec![],
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
        s.contains("git is writing on thistledown right now; retry once it is done"),
        "{s}"
    );
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
        last_sync: None,
        markers: vec![],
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
        addresses: vec![],
        replaced: vec![],
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

#[test]
fn a_deleted_unit_says_what_it_would_remove_and_that_it_needs_confirmation() {
    let mut on_home = unit(
        "work/gone",
        Decision::DeletedOn {
            host: "home".into(),
        },
    );
    on_home.leaf_files = 3;
    let mut on_leaf = unit(
        "work/left",
        Decision::DeletedOn {
            host: "leaf".into(),
        },
    );
    on_leaf.home_files = 1;
    let p = Plan {
        me: "leaf".into(),
        home: "home".into(),
        dry_run: false,
        units: vec![on_home, on_leaf],
        refused: vec![],
        orphaned: vec![],
        timings: Timings::default(),
    };
    let text = plan(&p, false, PLAIN);
    for want in [
        "deleted on home — would remove 3 files on leaf; requires explicit confirmation",
        "deleted on leaf — would remove 1 file on home; requires explicit confirmation",
        "deleted, needs confirmation: 2 units",
    ] {
        assert!(text.contains(want), "{want:?} not in:\n{text}");
    }
    assert!(
        plan(&p, false, COLOR).contains(
            "\x1b[31mdeleted on home — would remove 3 files on leaf; requires explicit confirmation\x1b[0m"
        ),
        "blocking, so red"
    );
    assert!(p.blocked());
}

#[test]
fn revoked_says_what_went_and_how_to_come_back() {
    use crate::daemon::wire::Revoked;
    let out = revoked(&Revoked {
        host: "birchbook".into(),
        cert: Some("sha256:ab".into()),
        uncollected: true,
    });
    assert!(
        out.starts_with("Revoked birchbook's certificate (sha256:ab).\n"),
        "{out}"
    );
    assert!(out.contains("uncollected"), "{out}");
    assert!(out.contains("`replicant approve birchbook`"), "{out}");
    let out = revoked(&Revoked {
        host: "birchbook".into(),
        cert: None,
        uncollected: false,
    });
    assert!(
        out.starts_with("birchbook had no certificate pinned.\n"),
        "{out}"
    );
    assert!(!out.contains("uncollected"), "{out}");
}

#[test]
fn a_units_notes_are_shown_when_verbose() {
    let mut p = a_plan();
    let note = "1 sockets, FIFOs or devices skipped, not synced: s";
    p.units[0].diff.as_mut().unwrap().notes = vec![note.into()];
    assert!(!plan(&p, false, PLAIN).contains(note));
    assert!(plan(&p, true, PLAIN).contains(note));
}

/// Guardrail 1 names the machine whose work is at risk, by direction.
#[test]
fn a_dirty_target_refusal_names_the_machine_and_the_remedy() {
    let mut p = a_plan();
    let dirty = Refusal {
        guard: Guard::DirtyTarget,
        unit: "work/e".into(),
        detail: vec!["plugin/README.md".into()],
    };
    let mut push = unit("work/e", Decision::Push);
    push.refusals = vec![dirty.clone()];
    let mut pull = unit("work/f", Decision::Pull);
    pull.refusals = vec![dirty];
    p.units = vec![push, pull];
    let s = plan(&p, false, PLAIN);
    assert!(
        s.contains("thistledown's copy has uncommitted work on these paths that this push would overwrite: commit or stash it on thistledown"),
        "{s}"
    );
    assert!(
        s.contains("birchbook's copy has uncommitted work on these paths that this pull would overwrite: commit or stash it on birchbook"),
        "{s}"
    );
}
