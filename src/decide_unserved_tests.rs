//! Review F5's rows, as amended: every reason × changed/unchanged, asserting
//! the row kind and whether it blocks; and markers.

use super::*;
use crate::record::RECORD_VERSION;
use crate::units::UnitKind;

const REASONS: [UnservedReason; 5] = [
    UnservedReason::Unsubscribed,
    UnservedReason::CapNarrowed,
    UnservedReason::RootMissing,
    UnservedReason::Excluded,
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
        home_name: "thistledown",
        rel: "work/x",
        path: "/Users/b/work2/x",
        reason,
        detail: "subscription work: no root in fleet.toml covers it; roots: work2",
        leaf,
        baseline: &base(),
        files,
        marker: m,
    })
}

fn changed() -> UnitState {
    UnitState::Present("edited".into())
}

#[test]
fn unsubscribed_unchanged_is_remove_and_does_not_block() {
    let d = run(UnservedReason::Unsubscribed, &base().leaf, 3, None);
    assert_eq!(d, Decision::Remove);
    assert!(!d.is_blocking());
}

#[test]
fn unsubscribed_changed_is_a_blocking_conflict_naming_the_way_out() {
    for leaf in [changed(), UnitState::Empty] {
        let d = run(UnservedReason::Unsubscribed, &leaf, 3, None);
        assert!(d.is_blocking(), "{d:?}");
        assert_eq!(d.direction(), None);
        assert_eq!(
            d,
            Decision::Conflict {
                why: "unsubscribed but changed here since baseline (3 files here); push \
                      first, or `replicant unsubscribe --force /Users/b/work2/x`"
                    .into()
            }
        );
    }
}

#[test]
fn policy_reasons_are_orphans_that_never_block() {
    for reason in [
        UnservedReason::CapNarrowed,
        UnservedReason::Excluded,
        UnservedReason::NotServed,
    ] {
        for (leaf, was_changed) in [(base().leaf, false), (changed(), true)] {
            let d = run(reason, &leaf, 1, None);
            assert_eq!(
                d,
                Decision::Orphaned {
                    reason,
                    changed: was_changed
                }
            );
            assert!(!d.is_blocking(), "{d:?}");
            assert_eq!(d.direction(), None, "an orphan never moves data");
        }
    }
    assert_eq!(
        run(UnservedReason::CapNarrowed, &base().leaf, 1, None).label(),
        "orphaned (cap narrowed)"
    );
    assert_eq!(
        run(UnservedReason::NotServed, &changed(), 1, None).label(),
        "orphaned, changed (not subscribed)"
    );
}

#[test]
fn a_missing_root_blocks_whether_or_not_the_leaf_changed() {
    for leaf in [base().leaf, changed()] {
        let d = run(UnservedReason::RootMissing, &leaf, 2, None);
        assert!(d.is_blocking());
        assert_eq!(d.direction(), None);
        assert_eq!(
            d,
            Decision::HomeRootMissing {
                why: "home's root for work/x is missing or renamed (subscription work: no \
                      root in fleet.toml covers it; roots: work2); fix fleet.toml on \
                      thistledown"
                    .into()
            }
        );
        assert_eq!(d.label(), "home root missing");
    }
}

#[test]
fn a_marker_is_never_remove_and_never_orphaned() {
    for dir in [Direction::Pull, Direction::Push] {
        let m = marker(dir);
        for reason in REASONS {
            for (leaf, since) in [(base().leaf, "unchanged"), (changed(), "changed")] {
                let d = run(reason, &leaf, 0, Some(&m));
                assert!(d.is_blocking(), "{reason}: {d:?}");
                let Decision::Conflict { why } = d else {
                    panic!("{reason}: {d:?}")
                };
                assert_eq!(
                    why,
                    format!(
                        "home no longer serves work/x ({reason}); an interrupted {dir} by \
                         birchbook is marked here; 0 files here, {since} since baseline; \
                         finish or roll back that sync first"
                    )
                );
            }
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
            "excluded by its root",
            "not subscribed"
        ]
    );
    for r in REASONS {
        let j = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<UnservedReason>(&j).unwrap(), r);
    }
    let orphan = Decision::Orphaned {
        reason: UnservedReason::CapNarrowed,
        changed: true,
    };
    let j = serde_json::to_string(&orphan).unwrap();
    assert_eq!(
        j,
        r#"{"decision":"orphaned","reason":"cap_narrowed","changed":true}"#
    );
    assert_eq!(serde_json::from_str::<Decision>(&j).unwrap(), orphan);
    assert_eq!(files_here(1), "1 file here");
}
