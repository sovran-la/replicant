//! `Plan::digest`: what a person confirmed (each unit's decision, refusals
//! and the paths it would delete, and the refused subscriptions) counts;
//! the bytes it would carry (states, creates and updates) do not.

use super::*;
use crate::guard::Guard;
use crate::marker::Direction;

fn diff(changes: &[&str]) -> Option<DiffSummary> {
    Some(DiffSummary {
        files_changed: changes.iter().filter(|c| !c.starts_with("- ")).count(),
        deleted: changes.iter().filter(|c| c.starts_with("- ")).count(),
        changes: changes.iter().map(|c| c.to_string()).collect(),
        notes: vec![],
    })
}

fn unit(rel: &str, decision: Decision, changes: &[&str]) -> PlannedUnit {
    PlannedUnit {
        rel: rel.into(),
        kind: UnitKind::Loose,
        decision,
        leaf: UnitState::Present("leaf-1".into()),
        home: UnitState::Present("home-1".into()),
        leaf_head: None,
        home_head: None,
        leaf_path: PathBuf::from("/leaf").join(rel),
        home_path: PathBuf::from("/home").join(rel),
        diff: diff(changes),
        refusals: vec![],
        leaf_files: 3,
        home_files: 3,
    }
}

fn plan(units: Vec<PlannedUnit>) -> Plan {
    Plan {
        me: "leaf".into(),
        home: "home".into(),
        dry_run: true,
        units,
        refused: vec![],
        orphaned: vec![],
        timings: Timings::default(),
    }
}

/// A pull of `work` creating, updating and deleting, and a push of `work/r`.
fn base() -> Plan {
    plan(vec![
        unit(
            "work",
            Decision::Pull,
            &["+ new.txt", "~ notes.txt", "- gone.txt", "- old/"],
        ),
        unit("work/r", Decision::Push, &["~ README", "a src/"]),
    ])
}

fn refusal(unit: &str) -> crate::guard::Refusal {
    crate::guard::Refusal {
        guard: Guard::DirtyTarget,
        unit: unit.into(),
        detail: vec!["README".into()],
    }
}

#[test]
fn states_and_what_moves_do_not_count() {
    let a = base();
    let mut b = base();
    b.units[0].leaf = UnitState::Present("leaf-2".into());
    b.units[0].home = UnitState::Empty;
    b.units[1].leaf = UnitState::Present("edited since".into());
    // Other creates, updates and attribute changes; the same deletions, in
    // another order.
    b.units[0].diff = diff(&["- old/", "~ data/blob.bin", "+ other.txt", "- gone.txt"]);
    b.units[1].diff = diff(&[]);
    // And what is only for show.
    b.units[0].leaf_files = 70_000;
    b.units[0].leaf_head = Some("abc".into());
    b.timings.total_ms = 20_000;
    b.dry_run = false;
    assert_eq!(a.digest(), b.digest());
}

#[test]
fn no_diff_and_an_empty_diff_are_the_same() {
    let mut a = base();
    let mut b = base();
    a.units[1].diff = None;
    b.units[1].diff = diff(&["+ x", "~ y"]);
    assert_eq!(a.digest(), b.digest());
}

#[test]
fn one_delete_more_or_fewer_counts() {
    let a = base();
    let mut more = base();
    more.units[0].diff = diff(&["+ new.txt", "~ notes.txt", "- gone.txt", "- old/", "- also"]);
    let mut fewer = base();
    fewer.units[0].diff = diff(&["+ new.txt", "~ notes.txt", "- gone.txt"]);
    // The same name, but a directory rather than a file.
    let mut other = base();
    other.units[0].diff = diff(&["+ new.txt", "~ notes.txt", "- gone.txt", "- old"]);
    // A delete in a unit that had none.
    let mut push = base();
    push.units[1].diff = diff(&["~ README", "- LICENSE"]);
    for p in [&more, &fewer, &other, &push] {
        assert_ne!(a.digest(), p.digest(), "{:#?}", p.units);
    }
}

#[test]
fn a_decision_counts() {
    let a = base();
    let flips = [
        Decision::Push,
        Decision::Resume {
            direction: Direction::Pull,
        },
        Decision::Remove,
        Decision::DeletedOn {
            host: "home".into(),
        },
        Decision::Conflict {
            why: "both changed since baseline".into(),
        },
        Decision::Nothing {
            why: "unchanged".into(),
        },
    ];
    for d in flips {
        let mut b = base();
        b.units[0].decision = d.clone();
        assert_ne!(a.digest(), b.digest(), "{d:?}");
    }
    // Whose deletion it is counts too.
    let mut x = base();
    x.units[0].decision = Decision::DeletedOn {
        host: "home".into(),
    };
    let mut y = base();
    y.units[0].decision = Decision::DeletedOn {
        host: "leaf".into(),
    };
    assert_ne!(x.digest(), y.digest());
}

#[test]
fn the_units_their_kinds_and_refusals_count() {
    let a = base();
    let mut added = base();
    added
        .units
        .push(unit("work/new", Decision::Pull, &["+ README"]));
    let mut dropped = base();
    dropped.units.pop();
    let mut kind = base();
    kind.units[1].kind = UnitKind::Repo;
    let mut refused = base();
    refused.units[1].refusals.push(refusal("work/r"));
    let mut sub = base();
    sub.refused.push(refusal("work/gone"));
    for p in [&added, &dropped, &kind, &refused, &sub] {
        assert_ne!(a.digest(), p.digest());
    }
}

#[test]
fn the_digest_is_stable() {
    assert_eq!(base().digest(), base().digest());
    assert_eq!(base().digest().len(), 64);
}
