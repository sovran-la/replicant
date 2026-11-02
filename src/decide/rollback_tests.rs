//! Review A2's rows, by themselves.

use super::*;
use std::path::PathBuf;

fn p(s: &str) -> UnitState {
    UnitState::Present(s.into())
}

fn rb() -> RolledBackPush {
    RolledBackPush {
        id: "leaf/20261101T120000Z".into(),
        unit: "work".into(),
        at: 1_762_000_000,
    }
}

fn held(
    d: Decision,
    leaf: &UnitState,
    home: &UnitState,
    r: Option<&RolledBackPush>,
    accepted: bool,
) -> Decision {
    let (lp, hp) = (PathBuf::from("/l/work"), PathBuf::from("/h/work"));
    hold_rolled_back(
        d,
        Held {
            rel: "work",
            home_name: "home",
            leaf,
            home,
            leaf_path: &lp,
            home_path: &hp,
            rolled_back: r,
            accepted,
        },
    )
}

#[test]
fn whatever_would_write_the_leaf_is_held_as_a_blocking_conflict() {
    let r = rb();
    for d in [
        Decision::Pull,
        Decision::Resume {
            direction: Direction::Pull,
        },
        Decision::DeletedOn {
            host: "home".into(),
        },
    ] {
        let got = held(d.clone(), &p("pushed"), &p("restored"), Some(&r), false);
        let Decision::Conflict { why } = &got else {
            panic!("{d:?} → {got:?}");
        };
        assert!(got.is_blocking());
        assert_eq!(got.direction(), None, "nothing moves");
        for part in [
            "home rolled back your push leaf/20261101T120000Z at 2025-11-01",
            "leaf /l/work",
            "home /h/work",
            "--accept-rollback work",
        ] {
            assert!(why.contains(part), "{part:?} not in {why}");
        }
    }
}

#[test]
fn an_unsubscribed_removal_is_held_until_accepted() {
    // Re-review R2: home serves the unit no more, so its side is empty.
    let r = rb();
    let got = held(
        Decision::Remove,
        &p("pushed"),
        &UnitState::Empty,
        Some(&r),
        false,
    );
    let Decision::Conflict { why } = &got else {
        panic!("{got:?}");
    };
    assert!(got.is_blocking());
    assert_eq!(got.direction(), None, "nothing moves");
    assert!(
        why.starts_with("home rolled back your push leaf/20261101T120000Z at "),
        "{why}"
    );
    assert!(
        why.ends_with("`sync --accept-rollback work` lets the removal delete the leaf's copy"),
        "{why}"
    );
    assert_eq!(
        held(
            Decision::Remove,
            &p("pushed"),
            &UnitState::Empty,
            Some(&r),
            true
        ),
        Decision::Remove,
        "accepted"
    );
    assert_eq!(
        held(
            Decision::Remove,
            &p("pushed"),
            &UnitState::Empty,
            None,
            false
        ),
        Decision::Remove,
        "never rolled back"
    );
    assert_eq!(
        held(
            Decision::Remove,
            &UnitState::Empty,
            &UnitState::Empty,
            Some(&r),
            false
        ),
        Decision::Remove,
        "nothing left here to remove"
    );
    // The other unserved rows write nothing on the leaf.
    for d in [
        Decision::Orphaned {
            reason: crate::decide::UnservedReason::CapNarrowed,
            changed: true,
        },
        Decision::HomeRootMissing { why: "gone".into() },
    ] {
        assert_eq!(
            held(d.clone(), &p("pushed"), &UnitState::Empty, Some(&r), false),
            d
        );
    }
}

#[test]
fn a_conflict_says_both() {
    let got = held(
        Decision::Conflict {
            why: "both changed since baseline".into(),
        },
        &p("a"),
        &p("b"),
        Some(&rb()),
        false,
    );
    let Decision::Conflict { why } = got else {
        panic!()
    };
    assert!(why.starts_with("home rolled back your push"), "{why}");
    assert!(why.ends_with("and both changed since baseline"), "{why}");
}

#[test]
fn what_does_not_write_the_leaf_is_left_as_the_table_says() {
    let r = rb();
    for d in [
        Decision::Push,
        Decision::Resume {
            direction: Direction::Push,
        },
        Decision::Nothing {
            why: "unchanged".into(),
        },
        Decision::DeletedOn {
            host: "leaf".into(),
        },
        Decision::Quarantined {
            by: "other".into(),
            since: 1,
        },
    ] {
        assert_eq!(held(d.clone(), &p("a"), &p("b"), Some(&r), false), d);
    }
}

#[test]
fn accepted_agreeing_or_never_rolled_back_is_the_tables_decision() {
    let r = rb();
    assert_eq!(
        held(Decision::Pull, &p("a"), &p("b"), Some(&r), true),
        Decision::Pull,
        "accepted"
    );
    let same = Decision::Nothing {
        why: "both changed identically".into(),
    };
    assert_eq!(
        held(same.clone(), &p("x"), &p("x"), Some(&r), false),
        same,
        "the leaf already holds home's copy: adopting it settles the rollback"
    );
    assert_eq!(
        held(Decision::Pull, &p("a"), &p("b"), None, false),
        Decision::Pull
    );
}
