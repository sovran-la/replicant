use super::*;
use crate::record::RECORD_VERSION;
use crate::units::UnitKind;

fn p(s: &str) -> UnitState {
    UnitState::Present(s.into())
}

const E: UnitState = UnitState::Empty;

fn base(leaf: UnitState, home: UnitState) -> Baseline {
    Baseline {
        version: RECORD_VERSION,
        rel: "work/x".into(),
        kind: UnitKind::Repo,
        leaf,
        home,
        at: 1,
    }
}

fn marker(host: &str, direction: Direction) -> Marker {
    Marker {
        version: RECORD_VERSION,
        rel: "work/x".into(),
        direction,
        host: host.into(),
        source_state: p("s"),
        started_at: 99,
    }
}

fn run(leaf: &UnitState, home: &UnitState, b: Option<&Baseline>) -> Decision {
    decide(Inputs {
        me: "birchbook",
        leaf,
        home,
        baseline: b,
        leaf_marker: None,
        home_marker: None,
    })
}

#[test]
fn no_baseline_leaf_empty_pulls() {
    assert_eq!(run(&E, &p("h"), None), Decision::Pull);
}

#[test]
fn no_baseline_home_empty_pushes() {
    assert_eq!(run(&p("l"), &E, None), Decision::Push);
}

#[test]
fn no_baseline_both_empty_is_nothing() {
    assert!(matches!(run(&E, &E, None), Decision::Nothing { .. }));
}

#[test]
fn no_baseline_both_populated_conflicts() {
    let d = run(&p("l"), &p("h"), None);
    assert!(matches!(d, Decision::Conflict { .. }), "{d:?}");
    assert!(d.is_blocking());
}

#[test]
fn no_baseline_both_populated_identical_is_nothing() {
    assert_eq!(
        run(&p("same"), &p("same"), None),
        Decision::Nothing {
            why: "identical".into()
        }
    );
}

#[test]
fn baseline_home_same_leaf_same_is_nothing() {
    let b = base(p("l"), p("h"));
    assert_eq!(
        run(&p("l"), &p("h"), Some(&b)),
        Decision::Nothing {
            why: "unchanged".into()
        }
    );
}

#[test]
fn baseline_home_changed_leaf_same_pulls() {
    let b = base(p("l"), p("h"));
    assert_eq!(run(&p("l"), &p("h2"), Some(&b)), Decision::Pull);
}

#[test]
fn baseline_home_same_leaf_changed_pushes() {
    let b = base(p("l"), p("h"));
    assert_eq!(run(&p("l2"), &p("h"), Some(&b)), Decision::Push);
}

#[test]
fn baseline_both_changed_conflicts() {
    let b = base(p("l"), p("h"));
    assert!(matches!(
        run(&p("l2"), &p("h2"), Some(&b)),
        Decision::Conflict { .. }
    ));
}

#[test]
fn baseline_both_changed_to_the_same_state_is_nothing() {
    let b = base(p("l"), p("h"));
    assert!(matches!(
        run(&p("z"), &p("z"), Some(&b)),
        Decision::Nothing { .. }
    ));
}

#[test]
fn baseline_with_deletion_on_leaf_pushes_the_delete() {
    let b = base(p("l"), p("h"));
    assert_eq!(run(&E, &p("h"), Some(&b)), Decision::Push);
}

#[test]
fn leaf_marker_resumes_pull_even_when_table_would_conflict() {
    let m = marker("birchbook", Direction::Pull);
    let b = base(p("l"), p("h"));
    let d = decide(Inputs {
        me: "birchbook",
        leaf: &p("half"),
        home: &p("h2"),
        baseline: Some(&b),
        leaf_marker: Some(&m),
        home_marker: None,
    });
    assert_eq!(
        d,
        Decision::Resume {
            direction: Direction::Pull
        }
    );
    assert!(!d.is_blocking(), "an interrupted sync is never a conflict");
}

#[test]
fn own_home_marker_resumes_push() {
    let m = marker("birchbook", Direction::Push);
    let d = decide(Inputs {
        me: "birchbook",
        leaf: &p("l"),
        home: &p("half"),
        baseline: None,
        leaf_marker: None,
        home_marker: Some(&m),
    });
    assert_eq!(
        d,
        Decision::Resume {
            direction: Direction::Push
        }
    );
    assert_eq!(d.direction(), Some(Direction::Push));
}

#[test]
fn other_leafs_home_marker_quarantines_even_over_own_leaf_marker() {
    let theirs = marker("twinoaks", Direction::Push);
    let mine = marker("birchbook", Direction::Pull);
    let d = decide(Inputs {
        me: "birchbook",
        leaf: &p("l"),
        home: &p("half"),
        baseline: None,
        leaf_marker: Some(&mine),
        home_marker: Some(&theirs),
    });
    assert_eq!(
        d,
        Decision::Quarantined {
            by: "twinoaks".into(),
            since: 99
        }
    );
    assert!(d.is_blocking());
    assert_eq!(d.direction(), None);
}

#[test]
fn labels_and_directions() {
    assert_eq!(Decision::Pull.direction(), Some(Direction::Pull));
    assert_eq!(Decision::Remove.direction(), None);
    assert_eq!(Decision::Remove.label(), "remove (unsubscribed)");
    assert_eq!(
        Decision::Resume {
            direction: Direction::Pull
        }
        .label(),
        "resume pull"
    );
    assert!(
        Decision::Conflict { why: "x".into() }
            .label()
            .starts_with("conflict")
    );
}
