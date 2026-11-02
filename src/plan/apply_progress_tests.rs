//! What a sync announces while it applies: one start and one end per unit
//! that does something, in plan order; nothing for
//! a plan that may not go.

use super::*;
use crate::marker::Direction;
use std::cell::RefCell;

/// `sync` as `leaf` with the system rsync, collecting every event; `on`
/// sees each as it is announced.
fn sync_watched(
    bed: &Bed,
    home: &Home,
    on: &dyn Fn(&ProgressEvent),
) -> (SyncReport, Vec<ProgressEvent>) {
    let link = InProcess {
        home,
        host: "leaf".into(),
        home_dir: bed.leaf_hd.clone(),
    };
    let cfg = bed.leaf_cfg();
    let dirs = (bed.leaf_dirs.state(), bed.leaf_dirs.scratch());
    let seen = RefCell::new(Vec::new());
    let emit = |e: ProgressEvent| {
        on(&e);
        seen.borrow_mut().push(e);
    };
    let progress = Progress::new(&emit);
    let ctx = ctx(bed, &cfg, &dirs, crate::plan::RSYNC);
    let r = sync_with_progress(&link, ctx, &SyncOpts::default(), &progress).unwrap();
    (r, seen.into_inner())
}

use crate::plan::apply::sync_with as sync_with_progress;

/// The events pair up, start then end, one pair per applied unit in the
/// report's order (which is plan order), numbered 1.. of `total`.
fn pairs_match(r: &SyncReport, events: &[ProgressEvent], total: usize) {
    assert_eq!(events.len(), 2 * r.applied.len(), "{events:#?}");
    for (i, (pair, a)) in events.chunks(2).zip(&r.applied).enumerate() {
        let ProgressEvent::UnitStarted {
            rel,
            action,
            index,
            total: t,
            ..
        } = &pair[0]
        else {
            panic!("not a start: {:?}", pair[0])
        };
        assert_eq!((rel, action, *index, *t), (&a.rel, &a.action, i + 1, total));
        let ProgressEvent::UnitDone {
            rel,
            action,
            files_changed,
            deleted,
            elapsed_ms,
            push,
            outcome,
            ..
        } = &pair[1]
        else {
            panic!("not an end: {:?}", pair[1])
        };
        assert_eq!(
            (rel, action, *files_changed, *deleted, push, outcome),
            (
                &a.rel,
                &a.action,
                a.files_changed,
                a.deleted,
                &a.push,
                &UnitOutcome::Applied
            )
        );
        assert!(*elapsed_ms < 120_000, "{elapsed_ms}");
    }
}

#[test]
fn each_unit_is_announced_as_it_starts_and_ends_in_plan_order() {
    let bed = Bed::enrolled();
    tree(&bed.home_hd.join("work"));
    let home = bed.home();
    let (r, events) = sync_watched(&bed, &home, &|_| {});
    assert_eq!(r.outcome, Outcome::Done);
    let rels: Vec<&str> = r.applied.iter().map(|a| a.rel.as_str()).collect();
    // Plan order: by path, a repo after the loose unit holding it.
    assert_eq!(
        rels,
        ["work", "work/alpha", "work/beta", "work/nested/gamma"]
    );
    pairs_match(&r, &events, 4);
    assert!(events.iter().all(|e| match e {
        ProgressEvent::UnitStarted { direction, .. }
        | ProgressEvent::UnitDone { direction, .. } => *direction == Some(Direction::Pull),
    }));
    assert!(r.applied.iter().any(|a| a.files_changed > 0));

    // Nothing left to do: nothing announced.
    let (r, events) = sync_watched(&bed, &home, &|_| {});
    assert!(r.applied.is_empty() && events.is_empty(), "{events:?}");
}

#[test]
fn an_adoption_is_announced_too() {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/r"));
    let copy = std::process::Command::new("cp")
        .arg("-Rp")
        .arg(bed.home_hd.join("work/r"))
        .arg(bed.leaf_hd.join("work/r"))
        .status()
        .unwrap();
    assert!(copy.success());
    let home = bed.home();
    let (r, events) = sync_watched(&bed, &home, &|_| {});
    assert_eq!(r.applied[0].action, "adopt");
    pairs_match(&r, &events, 1);
    assert!(matches!(
        &events[0],
        ProgressEvent::UnitStarted {
            direction: None,
            ..
        }
    ));
}

#[test]
fn a_plan_that_may_not_go_announces_nothing() {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/r"));
    let l = testutil::repo(&bed.leaf_hd.join("work/r"));
    testutil::git(&l, &["commit", "-q", "--allow-empty", "-m", "leaf only"]);
    let home = bed.home();
    let (r, events) = sync_watched(&bed, &home, &|e| panic!("announced {e:?}"));
    assert!(
        matches!(r.outcome, Outcome::Blocked { .. }),
        "{:?}",
        r.outcome
    );
    assert!(events.is_empty());
}
