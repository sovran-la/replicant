//! Home's apply requests through its handler: who may write, what each one
//! writes, and what it refuses.

use super::*;
use crate::baseline::Baseline;
use crate::marker::{Direction, Marker};
use crate::record::RECORD_VERSION;
use crate::state::UnitState;
use crate::units::UnitKind;

/// `leaf` and `other`, both subscribed to `work`.
fn two() -> Bed {
    let bed = Bed::enrolled();
    let hd = bed.leaf_hd.display().to_string();
    let mut text = std::fs::read_to_string(bed.home_dirs.fleet()).unwrap();
    text.push_str(&format!(
        "\n[hosts.other]\nhome_dir = \"{hd}\"\n\n[subscriptions.other]\npaths = [\"work\"]\n"
    ));
    std::fs::write(bed.home_dirs.fleet(), text).unwrap();
    bed
}

fn as_host(
    home: &Home,
    bed: &Bed,
    host: &str,
    r: HomeRequest,
) -> std::result::Result<HomeReply, String> {
    call(home, host, &bed.leaf_hd, r).result
}

fn lease(home: &Home, bed: &Bed, host: &str) -> u64 {
    match as_host(
        home,
        bed,
        host,
        HomeRequest::Acquire {
            no_wait: true,
            apply: true,
        },
    ) {
        Ok(HomeReply::Lease { lease, .. }) => lease,
        other => panic!("{other:?}"),
    }
}

fn begin(lease: u64, rel: &str) -> HomeRequest {
    HomeRequest::BeginPush {
        lease,
        rel: rel.into(),
        source_state: UnitState::Present("s".into()),
        is_file: false,
    }
}

fn markers(bed: &Bed) -> Vec<Marker> {
    crate::marker::store(&bed.home_dirs.state()).all().unwrap()
}

#[test]
fn every_write_needs_the_callers_own_lease_at_the_head() {
    let bed = two();
    let home = bed.home();
    let b = Baseline {
        version: RECORD_VERSION,
        rel: "work".into(),
        kind: UnitKind::Loose,
        leaf: UnitState::Empty,
        home: UnitState::Empty,
        at: 1,
    };
    let writes = |lease: u64| {
        vec![
            begin(lease, "work"),
            HomeRequest::Rehash {
                lease,
                rel: "work".into(),
                kind: UnitKind::Loose,
                union: vec![],
            },
            HomeRequest::Agree {
                lease,
                rel: "work".into(),
                baseline: Some(b.clone()),
                rollback_accepted: false,
            },
            HomeRequest::Settle {
                lease,
                rel: "work".into(),
                direction: Some(Direction::Push),
                detail: "x".into(),
                changes: vec![],
            },
        ]
    };
    for r in writes(999) {
        let e = as_host(&home, &bed, "leaf", r).unwrap_err();
        assert!(e.contains("does not hold the head"), "{e}");
    }
    // A dry run's entry, even at the head, writes nothing.
    let dry = match as_host(
        &home,
        &bed,
        "leaf",
        HomeRequest::Acquire {
            no_wait: true,
            apply: false,
        },
    ) {
        Ok(HomeReply::Lease { lease, .. }) => lease,
        other => panic!("{other:?}"),
    };
    for r in writes(dry) {
        let e = as_host(&home, &bed, "leaf", r).unwrap_err();
        assert!(e.contains("writes nothing"), "{e}");
    }
    home.queue.release(dry).unwrap();
    let theirs = lease(&home, &bed, "other");
    for r in writes(theirs) {
        let e = as_host(&home, &bed, "leaf", r).unwrap_err();
        assert!(e.contains("other's, not leaf's"), "{e}");
    }
    assert!(markers(&bed).is_empty());
    assert!(
        leaf_baselines(&bed.home_dirs.state(), "leaf")
            .all::<Baseline>()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_push_marker_is_kept_on_resume_and_cleared_only_by_its_host() {
    let bed = two();
    let home = bed.home();
    let l = lease(&home, &bed, "leaf");
    let begun = as_host(&home, &bed, "leaf", begin(l, "work/r")).unwrap();
    let HomeReply::Begun { push } = begun else {
        panic!("{begun:?}")
    };
    let first = markers(&bed);
    assert_eq!(first[0].push.as_deref(), Some(push.as_str()));
    assert_eq!(first.len(), 1);
    assert_eq!(
        (first[0].host.as_str(), first[0].direction),
        ("leaf", Direction::Push)
    );
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let again = as_host(&home, &bed, "leaf", begin(l, "work/r")).unwrap();
    assert!(
        matches!(again, HomeReply::Begun { push: ref p } if *p == push),
        "one push"
    );
    assert_eq!(markers(&bed), first, "a resume keeps the marker it resumes");
    let m: crate::daemon::home::Push = crate::daemon::home::pushes(&bed.home_dirs.state())
        .get(&push)
        .unwrap()
        .unwrap();
    assert_eq!(
        m.runs.len(),
        2,
        "the resume is a second run of the same push"
    );
    // The loose unit above the leaf's own marker may go too, with its own.
    as_host(&home, &bed, "leaf", begin(l, "work")).unwrap();
    assert_eq!(markers(&bed).len(), 2);
    home.queue.release(l).unwrap();

    // `other` is quarantined above, at and below the marked path.
    let o = lease(&home, &bed, "other");
    for rel in ["work", "work/r", "work/r/sub"] {
        let e = as_host(&home, &bed, "other", begin(o, rel)).unwrap_err();
        assert!(e.contains("quarantined"), "{rel}: {e}");
    }
    let e = as_host(
        &home,
        &bed,
        "other",
        HomeRequest::Settle {
            lease: o,
            rel: "work/r".into(),
            direction: Some(Direction::Push),
            detail: "x".into(),
            changes: vec![],
        },
    )
    .unwrap_err();
    assert!(e.contains("leaf's, not other's"), "{e}");
    home.queue.release(o).unwrap();

    let l = lease(&home, &bed, "leaf");
    for rel in ["work/r", "work"] {
        as_host(
            &home,
            &bed,
            "leaf",
            HomeRequest::Settle {
                lease: l,
                rel: rel.into(),
                direction: Some(Direction::Push),
                detail: "push 1 files 0 del ok".into(),
                changes: vec![],
            },
        )
        .unwrap();
    }
    assert!(markers(&bed).is_empty());
    let log = home.log(&Filter::default()).unwrap();
    let last = log.last().unwrap();
    assert_eq!(
        (last.event.as_str(), last.host.as_str(), last.path.as_str()),
        ("sync", "leaf", "work")
    );
}

#[test]
fn a_push_outside_what_home_serves_is_refused() {
    let bed = two();
    let home = bed.home();
    let l = lease(&home, &bed, "leaf");
    for rel in ["elsewhere/x", "../work", "/abs"] {
        let e = as_host(&home, &bed, "leaf", begin(l, rel)).unwrap_err();
        assert!(e.contains("not under") || e.contains("path"), "{rel}: {e}");
    }
    assert!(markers(&bed).is_empty());
}

#[test]
fn agree_writes_and_drops_home_s_copy_of_the_baseline() {
    let bed = two();
    let home = bed.home();
    let l = lease(&home, &bed, "leaf");
    let b = Baseline {
        version: RECORD_VERSION,
        rel: "work/r".into(),
        kind: UnitKind::Repo,
        leaf: UnitState::Present("l".into()),
        home: UnitState::Present("h".into()),
        at: 5,
    };
    let agree = |rel: &str, baseline: Option<Baseline>| HomeRequest::Agree {
        lease: l,
        rel: rel.into(),
        baseline,
        rollback_accepted: false,
    };
    let e = as_host(&home, &bed, "leaf", agree("work/other", Some(b.clone()))).unwrap_err();
    assert!(e.contains("sent as work/other"), "{e}");
    as_host(&home, &bed, "leaf", agree("work/r", Some(b.clone()))).unwrap();
    let store = leaf_baselines(&bed.home_dirs.state(), "leaf");
    assert_eq!(store.get::<Baseline>("work/r").unwrap(), Some(b));
    assert!(
        leaf_baselines(&bed.home_dirs.state(), "other")
            .all::<Baseline>()
            .unwrap()
            .is_empty(),
        "per host"
    );
    as_host(&home, &bed, "leaf", agree("work/r", None)).unwrap();
    assert_eq!(store.get::<Baseline>("work/r").unwrap(), None);
}

#[path = "home_apply_more_tests.rs"]
mod more;
