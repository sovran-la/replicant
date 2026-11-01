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
        home_name: "thistledown",
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

/// F4: this test used to assert `Push`, i.e. a vanished leaf unit sent a
/// whole-unit `rsync --delete` into home. PLAN rev 5's table makes it
/// "deleted on X", never a plain push.
#[test]
fn baseline_with_deletion_on_leaf_is_deleted_on_the_leaf_not_a_push() {
    let b = base(p("l"), p("h"));
    let d = run(&E, &p("h"), Some(&b));
    assert_eq!(
        d,
        Decision::DeletedOn {
            host: "birchbook".into()
        }
    );
    assert_eq!(
        d.direction(),
        None,
        "apply must not have a direction to take"
    );
    assert!(d.is_blocking());
    assert_eq!(d.label(), "deleted on birchbook");
}

#[test]
fn baseline_with_deletion_on_home_is_deleted_on_home_not_a_pull() {
    let b = base(p("l"), p("h"));
    let d = run(&p("l"), &E, Some(&b));
    assert_eq!(
        d,
        Decision::DeletedOn {
            host: "thistledown".into()
        }
    );
    assert_eq!(d.direction(), None);
    assert!(d.is_blocking());
}

/// Every other row stays as it was: only Present → Empty against an
/// unchanged, populated other side is a deletion.
#[test]
fn deleted_on_is_only_present_to_empty_against_an_unchanged_side() {
    let b = base(p("l"), p("h"));
    assert!(
        matches!(run(&E, &p("h2"), Some(&b)), Decision::Conflict { .. }),
        "vanished here, changed there: still a conflict"
    );
    assert!(
        matches!(run(&p("l2"), &E, Some(&b)), Decision::Conflict { .. }),
        "vanished on home, changed here: still a conflict"
    );
    assert!(
        matches!(run(&E, &E, Some(&b)), Decision::Nothing { .. }),
        "gone on both sides: both changed identically"
    );
    // Empty at baseline is not "vanished": it was never there.
    let from_empty = base(E, E);
    assert_eq!(run(&p("l"), &E, Some(&from_empty)), Decision::Push);
    assert_eq!(run(&E, &p("h"), Some(&from_empty)), Decision::Pull);
    // Vanished on one side while the other was already empty at baseline
    // and still is: there is nothing on the other side to remove.
    let half = base(E, p("h"));
    assert_eq!(run(&E, &E, Some(&half)), Decision::Pull);
    // No baseline: the first-sync rows, unchanged.
    assert_eq!(run(&E, &p("h"), None), Decision::Pull);
    assert_eq!(run(&p("l"), &E, None), Decision::Push);
}

#[test]
fn markers_still_win_over_a_deletion() {
    let b = base(p("l"), p("h"));
    let m = marker("birchbook", Direction::Pull);
    let d = decide(Inputs {
        me: "birchbook",
        home_name: "thistledown",
        leaf: &E,
        home: &p("h"),
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
}

#[test]
fn leaf_marker_resumes_pull_even_when_table_would_conflict() {
    let m = marker("birchbook", Direction::Pull);
    let b = base(p("l"), p("h"));
    let d = decide(Inputs {
        me: "birchbook",
        home_name: "thistledown",
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
        home_name: "thistledown",
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
        home_name: "thistledown",
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
