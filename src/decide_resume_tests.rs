//! Apply's two marker rows: a resume whose source vanished is a conflict,
//! not a delete; and a leaf's own interrupted removal is finished, not a
//! conflict.

use super::*;
use crate::decide::unserved::{UnservedInputs, UnservedReason, decide_unserved};
use crate::record::RECORD_VERSION;
use crate::units::UnitKind;

fn p(s: &str) -> UnitState {
    UnitState::Present(s.into())
}

fn marker(host: &str, direction: Direction, source: UnitState) -> Marker {
    Marker {
        version: RECORD_VERSION,
        rel: "work/x".into(),
        direction,
        host: host.into(),
        source_state: source,
        started_at: 99,
    }
}

fn with(leaf: &UnitState, home: &UnitState, lm: Option<&Marker>, hm: Option<&Marker>) -> Decision {
    decide(Inputs {
        rel: "work/x",
        me: "birchbook",
        home_name: "thistledown",
        leaf,
        home,
        baseline: None,
        leaf_marker: lm,
        home_marker: hm,
    })
}

#[test]
fn a_resume_whose_source_vanished_is_a_conflict_not_a_delete() {
    let pull = marker("birchbook", Direction::Pull, p("home-then"));
    let d = with(&p("half"), &UnitState::Empty, Some(&pull), None);
    assert!(
        matches!(&d, Decision::Conflict { why } if why.contains("vanished")),
        "{d:?}"
    );
    assert!(d.is_blocking());
    assert_eq!(d.direction(), None);

    let push = marker("birchbook", Direction::Push, p("leaf-then"));
    let d = with(&UnitState::Empty, &p("half"), None, Some(&push));
    assert!(
        matches!(&d, Decision::Conflict { why } if why.contains("vanished")),
        "{d:?}"
    );
}

#[test]
fn a_resumed_deletion_with_an_empty_source_is_still_a_resume() {
    // The marker says the source was already empty: a confirmed deletion.
    let push = marker("birchbook", Direction::Push, UnitState::Empty);
    assert_eq!(
        with(&UnitState::Empty, &p("half-deleted"), None, Some(&push)),
        Decision::Resume {
            direction: Direction::Push
        }
    );
    let pull = marker("birchbook", Direction::Pull, UnitState::Empty);
    assert_eq!(
        with(&p("half-deleted"), &UnitState::Empty, Some(&pull), None),
        Decision::Resume {
            direction: Direction::Pull
        }
    );
}

#[test]
fn a_resume_with_its_source_present_is_unchanged() {
    let pull = marker("birchbook", Direction::Pull, p("home-then"));
    assert_eq!(
        with(&p("half"), &p("home-now"), Some(&pull), None),
        Decision::Resume {
            direction: Direction::Pull
        }
    );
}

fn unserved(reason: UnservedReason, m: Option<&Marker>) -> Decision {
    let b = Baseline {
        version: RECORD_VERSION,
        rel: "work/x".into(),
        kind: UnitKind::Repo,
        leaf: p("at-baseline"),
        home: p("h"),
        at: 1,
    };
    decide_unserved(UnservedInputs {
        home_name: "thistledown",
        rel: "work/x",
        path: "/Users/b/work/x",
        reason,
        detail: "",
        leaf: &p("half-removed"),
        baseline: &b,
        files: 2,
        marker: m,
        me: "birchbook",
    })
}

#[test]
fn an_own_interrupted_removal_is_finished() {
    let mine = marker("birchbook", Direction::Pull, UnitState::Empty);
    assert_eq!(
        unserved(UnservedReason::Unsubscribed, Some(&mine)),
        Decision::Remove
    );
}

#[test]
fn any_other_marker_over_an_unserved_path_is_still_a_conflict() {
    let cases = [
        (
            marker("twinoaks", Direction::Pull, UnitState::Empty),
            UnservedReason::Unsubscribed,
        ),
        (
            marker("birchbook", Direction::Push, UnitState::Empty),
            UnservedReason::Unsubscribed,
        ),
        (
            marker("birchbook", Direction::Pull, p("s")),
            UnservedReason::Unsubscribed,
        ),
        (
            marker("birchbook", Direction::Pull, UnitState::Empty),
            UnservedReason::CapNarrowed,
        ),
    ];
    for (m, reason) in cases {
        let d = unserved(reason, Some(&m));
        assert!(
            matches!(d, Decision::Conflict { .. }),
            "{m:?} {reason:?}: {d:?}"
        );
    }
    let mut elsewhere = marker("birchbook", Direction::Pull, UnitState::Empty);
    elsewhere.rel = "work".into();
    assert!(matches!(
        unserved(UnservedReason::Unsubscribed, Some(&elsewhere)),
        Decision::Conflict { .. }
    ));
}

#[test]
fn a_marker_above_or_below_the_unit_says_nothing_about_its_source() {
    // A loose unit `work` holding the marked repo `work/x`: its own source
    // being empty is not the marked sync's source vanishing.
    let pull = marker("birchbook", Direction::Pull, p("home-then"));
    let d = decide(Inputs {
        rel: "work",
        me: "birchbook",
        home_name: "thistledown",
        leaf: &p("loose"),
        home: &UnitState::Empty,
        baseline: None,
        leaf_marker: Some(&pull),
        home_marker: None,
    });
    assert_eq!(
        d,
        Decision::Resume {
            direction: Direction::Pull
        }
    );
}
