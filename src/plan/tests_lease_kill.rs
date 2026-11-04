//! Review F3, second half: a lease lost while a plan's dry runs run fails
//! the plan. A dry run is a walk and home's manifest of the unit, held here
//! until home has dropped the queue entry and the leaf's heartbeat heard.

use super::*;
use crate::daemon::home::Home;
use crate::testutil::{self, Bed, InProcess};
use std::time::{Duration, Instant};

pub(crate) fn wait_for_pid(p: &Path) -> i32 {
    let by = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(t) = std::fs::read_to_string(p)
            && let Ok(n) = t.trim().parse()
        {
            return n;
        }
        assert!(Instant::now() < by, "{} never written", p.display());
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Drop whatever entry heads home's queue once `pid_file` exists.
pub(crate) fn drop_head_when_running<'s>(
    s: &'s std::thread::Scope<'s, '_>,
    home: &'s Home,
    pid_file: std::path::PathBuf,
) -> std::thread::ScopedJoinHandle<'s, ()> {
    s.spawn(move || {
        wait_for_pid(&pid_file);
        let head = home.queue.view().first().expect("an entry is held").id;
        home.queue.release(head).unwrap();
    })
}

#[test]
fn a_lost_lease_during_the_dry_runs_fails_the_plan() {
    let bed = Bed::enrolled();
    testutil::hand_edit(
        &bed.home_dirs.fleet(),
        "listen = ",
        "lease_secs = 1\nlisten = ",
    );
    testutil::repo(&bed.leaf_hd.join("work/r"));
    let home = bed.home();
    let pid_file = bed.tmp.path().join("running");
    let inner = InProcess {
        home: &home,
        host: "leaf".into(),
        home_dir: bed.leaf_hd.clone(),
    };
    // Home's answer for the unit waits until the entry is gone and the
    // leaf's heartbeat has had time to hear so.
    let link =
        crate::native::tests_fixture::Faulty::new(inner, crate::native::tests_fixture::Fault::None)
            .on_call(Box::new(|r, _| {
                if matches!(r, crate::daemon::wire::HomeRequest::Manifest { .. }) {
                    testutil::write(&pid_file, &std::process::id().to_string());
                    let by = Instant::now() + Duration::from_secs(15);
                    while !home.queue.view().is_empty() && Instant::now() < by {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    std::thread::sleep(Duration::from_millis(1500));
                }
                Ok(())
            }));
    let cfg = bed.leaf_cfg();
    let state = bed.leaf_dirs.state();
    let ctx = LeafCtx {
        me: "leaf",
        home_dir: &bed.leaf_hd,
        cfg: &cfg,
        state_dir: &state,
        plane: Default::default(),
        accept_rollback: &[],
    };
    let started = Instant::now();
    let err = std::thread::scope(|s| {
        let dropper = drop_head_when_running(s, &home, pid_file.clone());
        let r = plan(&link, ctx, true, false);
        dropper.join().unwrap();
        r
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("lease lost"), "{err}");
    // Home's answer is held up to 15s; the plan did not wait it out.
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert!(home.queue.view().is_empty());
}
