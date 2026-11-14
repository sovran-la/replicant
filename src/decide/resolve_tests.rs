use super::*;
use crate::decide::Decision;
use crate::record::RECORD_VERSION;

fn present(h: &str) -> UnitState {
    UnitState::Present(h.into())
}

fn conflict(why: &str) -> Decision {
    Decision::Conflict { why: why.into() }
}

fn marker(rel: &str) -> Marker {
    Marker {
        version: RECORD_VERSION,
        rel: rel.into(),
        direction: Direction::Pull,
        host: "leaf".into(),
        source_state: present("s"),
        started_at: 1,
        push: None,
        backup: None,
    }
}

struct Case {
    table: Decision,
    held: Decision,
    leaf: UnitState,
    home: UnitState,
    leaf_marker: Option<Marker>,
    home_marker: Option<Marker>,
}

impl Case {
    fn both_changed() -> Self {
        let c = conflict("both changed since baseline");
        Case {
            table: c.clone(),
            held: c,
            leaf: present("l"),
            home: present("h"),
            leaf_marker: None,
            home_marker: None,
        }
    }

    fn check(&self) -> bool {
        resolvable(Resolvable {
            rel: "work/r",
            table: &self.table,
            held: &self.held,
            leaf: &self.leaf,
            home: &self.home,
            leaf_marker: self.leaf_marker.as_ref(),
            home_marker: self.home_marker.as_ref(),
        })
    }
}

#[test]
fn the_tables_conflicts_with_both_sides_populated_are_resolvable() {
    assert!(Case::both_changed().check());
    let c = conflict("no baseline, both populated");
    let none = Case {
        table: c.clone(),
        held: c,
        ..Case::both_changed()
    };
    assert!(none.check());
}

#[test]
fn a_decision_that_is_not_a_conflict_is_not_resolvable() {
    for d in [
        Decision::Push,
        Decision::Pull,
        Decision::Remove,
        Decision::Nothing {
            why: "unchanged".into(),
        },
        Decision::Quarantined {
            by: "other".into(),
            since: 1,
        },
    ] {
        let c = Case {
            table: d.clone(),
            held: d.clone(),
            ..Case::both_changed()
        };
        assert!(!c.check(), "{d:?}");
    }
}

/// A rolled-back push holds its unit as a conflict with its own answer,
/// `--accept-rollback`; so does a table conflict it wraps.
#[test]
fn a_conflict_a_rolled_back_push_holds_is_not_resolvable() {
    let held = Case {
        table: Decision::Pull,
        held: conflict("home rolled back your push"),
        ..Case::both_changed()
    };
    assert!(!held.check());
    let wrapped = Case {
        held: conflict("home rolled back your push; and both changed since baseline"),
        ..Case::both_changed()
    };
    assert!(!wrapped.check());
}

/// Taking an empty side would delete the whole unit on the other.
#[test]
fn an_empty_side_is_not_resolvable() {
    let leaf_empty = Case {
        leaf: UnitState::Empty,
        ..Case::both_changed()
    };
    assert!(!leaf_empty.check());
    let home_empty = Case {
        home: UnitState::Empty,
        ..Case::both_changed()
    };
    assert!(!home_empty.check());
}

/// An interrupted sync of exactly this unit says what to do instead; a
/// marker above or below it does not.
#[test]
fn an_own_marker_makes_it_not_resolvable_and_another_paths_does_not() {
    let leaf = Case {
        leaf_marker: Some(marker("work/r")),
        ..Case::both_changed()
    };
    assert!(!leaf.check());
    let home = Case {
        home_marker: Some(marker("work/r")),
        ..Case::both_changed()
    };
    assert!(!home.check());
    let elsewhere = Case {
        leaf_marker: Some(marker("work")),
        home_marker: Some(marker("work/r/inner")),
        ..Case::both_changed()
    };
    assert!(elsewhere.check());
}

#[test]
fn take_is_a_direction_and_reads_back_from_json() {
    assert_eq!(Take::Leaf.direction(), Direction::Push);
    assert_eq!(Take::Home.direction(), Direction::Pull);
    let r = Resolution {
        rel: "work/r".into(),
        take: Take::Home,
    };
    let text = serde_json::to_string(&r).unwrap();
    assert_eq!(text, r#"{"rel":"work/r","take":"home"}"#);
    assert_eq!(serde_json::from_str::<Resolution>(&text).unwrap(), r);
}

#[test]
fn a_resolved_decision_has_its_direction_and_label() {
    let leaf = Decision::Resolved { take: Take::Leaf };
    let home = Decision::Resolved { take: Take::Home };
    assert_eq!(leaf.direction(), Some(Direction::Push));
    assert_eq!(home.direction(), Some(Direction::Pull));
    assert!(!leaf.is_blocking() && leaf.applies());
    assert_eq!(leaf.label(), "push (resolved: this copy)");
    assert_eq!(home.label(), "pull (resolved: home's copy)");
}
