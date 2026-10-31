//! Plan tests beyond the table rows: removal, `--as`, the queue, enrollment.

use super::*;
use std::time::Duration;

fn put_marker(dir: &Path, host: &str, direction: Direction) {
    crate::marker::store(dir)
        .put(&Marker {
            version: RECORD_VERSION,
            rel: "work/r".into(),
            direction,
            host: host.into(),
            source_state: UnitState::Present("s".into()),
            started_at: 7,
        })
        .unwrap();
}

#[test]
fn interrupted_marker_rows_resume_or_quarantine() {
    let bed = Bed::enrolled();
    diverged(&bed);
    put_marker(&bed.leaf_dirs.state(), "leaf", Direction::Pull);
    let p = dry(&bed);
    let r = unit(&p, "work/r");
    assert_eq!(
        r.decision,
        Decision::Resume {
            direction: Direction::Pull
        },
        "never a conflict"
    );
    assert!(r.diff.is_some(), "--dry-run shows what resuming would do");

    put_marker(&bed.home_dirs.state(), "leaf", Direction::Push);
    std::fs::remove_dir_all(bed.leaf_dirs.state().join("markers")).unwrap();
    assert_eq!(
        unit(&dry(&bed), "work/r").decision,
        Decision::Resume {
            direction: Direction::Push
        }
    );

    put_marker(&bed.home_dirs.state(), "twinoaks", Direction::Push);
    let q = dry(&bed);
    assert_eq!(
        unit(&q, "work/r").decision,
        Decision::Quarantined {
            by: "twinoaks".into(),
            since: 7
        }
    );
    assert!(unit(&q, "work/r").diff.is_none());
}

#[test]
fn unsubscribed_baseline_is_removed_through_guard1() {
    let bed = Bed::enrolled();
    let text = std::fs::read_to_string(bed.home_dirs.fleet()).unwrap();
    std::fs::write(
        bed.home_dirs.fleet(),
        text.replace(
            "paths = [\"work\"]",
            "paths = [\"work\"]\nexcept = [\"work/old\"]",
        ),
    )
    .unwrap();
    let old = testutil::repo(&bed.leaf_hd.join("work/old"));
    testutil::write(&old.join("README"), "uncommitted\n");
    put_baseline(
        &bed,
        "work/old",
        UnitState::Present("x".into()),
        UnitState::Present("y".into()),
    );
    let p = dry(&bed);
    let r = unit(&p, "work/old");
    assert_eq!(r.decision, Decision::Remove);
    assert!(r.diff.as_ref().unwrap().deleted > 0);
    assert_eq!(r.refusals[0].guard, Guard::DirtyTarget);
    assert!(old.join("README").exists());
}

#[test]
fn as_maps_the_subscription_to_a_different_local_path() {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/r"));
    let work2 = bed.tmp.path().join("work2");
    let mut cfg = bed.leaf_cfg();
    cfg.as_local.insert("work".into(), work2.clone());
    let p = go(&bed, &bed.home(), &cfg, true).unwrap();
    assert_eq!(unit(&p, "work/r").leaf_path, work2.join("r"));
    assert_eq!(unit(&p, "work/r").decision, Decision::Pull);
    assert!(
        !work2.exists(),
        "a dry run creates nothing, not even the target dir"
    );
}

#[test]
fn dry_run_takes_and_releases_a_queue_entry_and_journals() {
    let bed = Bed::enrolled();
    let home = bed.home();
    go(&bed, &home, &bed.leaf_cfg(), true).unwrap();
    assert!(home.queue.view().is_empty(), "released");
    let log = home.log(&Default::default()).unwrap();
    assert_eq!(log.last().unwrap().event, "dry-run");

    let held = home
        .queue
        .acquire(
            "other",
            EntryKind::Hold,
            vec![],
            Duration::from_secs(30),
            false,
            Duration::from_secs(1),
        )
        .unwrap();
    let link = InProcess {
        home: &home,
        host: "leaf".into(),
        home_dir: bed.leaf_hd.clone(),
    };
    let cfg = bed.leaf_cfg();
    let state = bed.leaf_dirs.state();
    let scratch = bed.leaf_dirs.scratch();
    let ctx = LeafCtx {
        me: "leaf",
        home_dir: &bed.leaf_hd,
        cfg: &cfg,
        state_dir: &state,
        scratch_dir: &scratch,
    };
    let err = plan(&link, ctx, true, true).unwrap_err().to_string();
    assert!(
        err.contains("queue busy") && err.contains("other hold"),
        "{err}"
    );
    home.queue.release(held).unwrap();
    // Status does not queue at all.
    go(&bed, &home, &cfg, false).unwrap();
}

#[test]
fn unenrolled_leaf_cannot_plan() {
    let bed = Bed::new("");
    let err = go(&bed, &bed.home(), &bed.leaf_cfg(), false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("not enrolled"), "{err}");
}
