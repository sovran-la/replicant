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
            push: None,
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
    let old = testutil::repo(&bed.leaf_hd.join("work/old"));
    testutil::write(&old.join("README"), "uncommitted\n");
    // The baseline records the leaf as it is now (review F5: `Remove` needs
    // leaf == baseline.leaf), dirty README included.
    let now = unit(
        &go(&bed, &bed.home(), &bed.leaf_cfg(), false).unwrap(),
        "work/old",
    )
    .leaf
    .clone();
    let text = std::fs::read_to_string(bed.home_dirs.fleet()).unwrap();
    std::fs::write(
        bed.home_dirs.fleet(),
        text.replace(
            "paths = [\"work\"]",
            "paths = [\"work\"]\nexcept = [\"work/old\"]",
        ),
    )
    .unwrap();
    put_baseline(&bed, "work/old", now, UnitState::Present("y".into()));
    let p = dry(&bed);
    let r = unit(&p, "work/old");
    assert_eq!(r.decision, Decision::Remove);
    assert!(r.diff.as_ref().unwrap().deleted > 0);
    assert_eq!(r.refusals[0].guard, Guard::DirtyTarget);
    assert!(old.join("README").exists());

    // A leaf that moved since the baseline is a conflict, not a delete.
    put_baseline(
        &bed,
        "work/old",
        UnitState::Present("x".into()),
        UnitState::Present("y".into()),
    );
    let r = dry(&bed);
    let r = unit(&r, "work/old");
    assert!(
        matches!(&r.decision, Decision::Conflict { why } if why.starts_with("unsubscribed but changed")),
        "{:?}",
        r.decision
    );
    assert!(r.diff.is_none());
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
        plane: crate::plan::xfer::test_plane(),
        accept_rollback: &[],
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

/// The leaf's link, but home forgets the entry (a restart, an expiry) as soon
/// as the plan asks for state, and the plan is held until a heartbeat has
/// been told so. Leases are shortened to one second so that happens quickly.
struct DropsEntry<'a> {
    inner: InProcess<'a>,
    lease: std::sync::Mutex<Option<u64>>,
    refused: std::sync::atomic::AtomicBool,
}

impl HomeLink for DropsEntry<'_> {
    fn call(&self, request: HomeRequest) -> Result<HomeReply> {
        use std::sync::atomic::Ordering;
        match request {
            HomeRequest::Acquire { .. } => match self.inner.call(request)? {
                HomeReply::Lease { lease, .. } => {
                    *self.lease.lock().unwrap() = Some(lease);
                    Ok(HomeReply::Lease {
                        lease,
                        lease_secs: 1,
                    })
                }
                other => Ok(other),
            },
            HomeRequest::State { .. } => {
                let id = self.lease.lock().unwrap().expect("acquired first");
                self.inner.home.queue.release(id).unwrap();
                let by = std::time::Instant::now() + Duration::from_secs(10);
                while !self.refused.load(Ordering::SeqCst) {
                    assert!(std::time::Instant::now() < by, "no heartbeat was refused");
                    std::thread::sleep(Duration::from_millis(5));
                }
                self.inner.call(request)
            }
            HomeRequest::Heartbeat { .. } => {
                let r = self.inner.call(request);
                if r.is_err() {
                    self.refused.store(true, Ordering::SeqCst);
                }
                r
            }
            other => self.inner.call(other),
        }
    }
}

#[test]
fn home_dropping_the_entry_mid_plan_fails_the_dry_run_loudly() {
    let bed = Bed::enrolled();
    diverged(&bed);
    let home = bed.home();
    let link = DropsEntry {
        inner: InProcess {
            home: &home,
            host: "leaf".into(),
            home_dir: bed.leaf_hd.clone(),
        },
        lease: Default::default(),
        refused: Default::default(),
    };
    let cfg = bed.leaf_cfg();
    let (state, scratch) = (bed.leaf_dirs.state(), bed.leaf_dirs.scratch());
    let ctx = LeafCtx {
        me: "leaf",
        home_dir: &bed.leaf_hd,
        cfg: &cfg,
        state_dir: &state,
        scratch_dir: &scratch,
        plane: crate::plan::xfer::test_plane(),
        accept_rollback: &[],
    };
    let err = plan(&link, ctx, true, false).unwrap_err().to_string();
    assert!(err.contains("lease lost"), "{err}");
    assert!(home.queue.view().is_empty());
    let log = home.log(&Default::default()).unwrap();
    assert!(
        !log.iter().any(|l| l.event == "dry-run"),
        "a void plan is not reported as a dry run: {log:?}"
    );
}

/// The leaf's link, with home refusing to journal the plan's report.
struct NoReport<'a>(InProcess<'a>);

impl HomeLink for NoReport<'_> {
    fn call(&self, request: HomeRequest) -> Result<HomeReply> {
        match request {
            HomeRequest::Report { .. } => Err(Error::Remote("journal disk full".into())),
            other => self.0.call(other),
        }
    }
}

#[test]
fn a_failed_final_report_does_not_fail_finished_work() {
    let bed = Bed::enrolled();
    diverged(&bed);
    let home = bed.home();
    let link = NoReport(InProcess {
        home: &home,
        host: "leaf".into(),
        home_dir: bed.leaf_hd.clone(),
    });
    let cfg = bed.leaf_cfg();
    let (state, scratch) = (bed.leaf_dirs.state(), bed.leaf_dirs.scratch());
    let ctx = LeafCtx {
        me: "leaf",
        home_dir: &bed.leaf_hd,
        cfg: &cfg,
        state_dir: &state,
        scratch_dir: &scratch,
        plane: crate::plan::xfer::test_plane(),
        accept_rollback: &[],
    };
    let p = plan(&link, ctx, true, false).expect("the plan stands");
    assert!(p.units.iter().any(|u| u.rel == "work/r"));
    assert!(home.queue.view().is_empty(), "still released");
}
