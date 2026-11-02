//! Review F5's rows: every reason × changed/unchanged, and markers.

use super::*;
use crate::record::RECORD_VERSION;
use crate::units::UnitKind;

const REASONS: [UnservedReason; 4] = [
    UnservedReason::Unsubscribed,
    UnservedReason::CapNarrowed,
    UnservedReason::RootMissing,
    UnservedReason::NotServed,
];

fn base() -> Baseline {
    Baseline {
        version: RECORD_VERSION,
        rel: "work/x".into(),
        kind: UnitKind::Repo,
        leaf: UnitState::Present("at-baseline".into()),
        home: UnitState::Present("h".into()),
        at: 1,
    }
}

fn marker(direction: Direction) -> Marker {
    Marker {
        version: RECORD_VERSION,
        rel: "work/x".into(),
        direction,
        host: "birchbook".into(),
        source_state: UnitState::Present("s".into()),
        started_at: 9,
    }
}

fn run(reason: UnservedReason, leaf: &UnitState, files: u64, m: Option<&Marker>) -> Decision {
    decide_unserved(UnservedInputs {
        rel: "work/x",
        reason,
        leaf,
        baseline: &base(),
        files,
        marker: m,
    })
}

fn why(d: Decision) -> String {
    match d {
        Decision::Conflict { why } => why,
        other => panic!("expected a conflict, got {other:?}"),
    }
}

#[test]
fn unsubscribed_and_unchanged_is_the_only_remove() {
    let same = base().leaf;
    for r in REASONS {
        let d = run(r, &same, 3, None);
        if r == UnservedReason::Unsubscribed {
            assert_eq!(d, Decision::Remove);
            assert!(!d.is_blocking());
        } else {
            let w = why(d);
            assert_eq!(
                w,
                format!(
                    "home no longer serves work/x ({r}); 3 files here, unchanged since baseline; \
                     re-subscribe, or unsubscribe to remove"
                )
            );
        }
    }
}

#[test]
fn changed_since_baseline_is_a_conflict_for_every_reason() {
    for leaf in [UnitState::Present("edited".into()), UnitState::Empty] {
        for r in REASONS {
            let d = run(r, &leaf, 1, None);
            assert!(d.is_blocking(), "{r}: {d:?}");
            assert_eq!(d.direction(), None, "a conflict never moves data");
            assert_eq!(
                why(d),
                format!(
                    "home no longer serves work/x ({r}); 1 file here, changed since baseline; \
                     re-subscribe, or unsubscribe to remove"
                )
            );
        }
    }
}

#[test]
fn a_marker_is_never_remove() {
    let same = base().leaf;
    for dir in [Direction::Pull, Direction::Push] {
        let m = marker(dir);
        for r in REASONS {
            let w = why(run(r, &same, 0, Some(&m)));
            assert!(
                w.contains(&format!(
                    "0 files here, unchanged since baseline; an interrupted {dir} by birchbook \
                     is marked here;"
                )),
                "{w}"
            );
        }
    }
}

#[test]
fn reasons_render_and_round_trip() {
    let shown: Vec<String> = REASONS.iter().map(|r| r.to_string()).collect();
    assert_eq!(
        shown,
        [
            "unsubscribed",
            "cap narrowed",
            "no root serves it",
            "not subscribed"
        ]
    );
    for r in REASONS {
        let j = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<UnservedReason>(&j).unwrap(), r);
    }
    assert_eq!(
        serde_json::to_string(&UnservedReason::CapNarrowed).unwrap(),
        "\"cap_narrowed\""
    );
}
