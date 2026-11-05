//! Review N1: the CLI hanging up mid-unit stops that unit's transfer, as a
//! lost lease does. The daemon side is the real `local::serve`; the CLI is
//! the other end of a socket pair, reading frames until `work/beta` starts
//! and its transfer is running (stalled: `tests_inject`), then closing. And
//! review P2: a hangup after the unit's transfer finished whole lets that
//! unit finish.

use super::fail::{resumes, stopped_at_beta};
use super::*;
use crate::daemon::local::{self, HANGUP};
use crate::daemon::remote::{recv_buffered, send};
use crate::daemon::wire::{
    HomeRequest, LOCAL_WIRE_VERSION, LocalCall, LocalFrame, LocalReply, LocalRequest,
};
use crate::native::tests_fixture::{Fault, Faulty};
use crate::plan::tests::inject::{How, Inject};
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[test]
fn the_cli_hanging_up_stops_the_units_transfer_and_the_sync() {
    let bed = Bed::enrolled();
    tree(&bed.home_hd.join("work"));
    let home = bed.home();
    let inj = Inject::new(&bed, "work/beta", How::Stalls);
    let link = inj.link(&bed, &home);
    let cfg = bed.leaf_cfg();
    let dirs = bed.leaf_dirs.state();
    let ctx = ctx(&bed, &cfg, &dirs);
    let report = Mutex::new(None);
    let (mut cli, daemon) = UnixStream::pair().unwrap();
    let started = Instant::now();
    let _dog = crate::testutil::watchdog(SYNC_LIMIT, "a sync whose CLI hung up".into());
    let mut seen = Vec::new();
    std::thread::scope(|s| {
        let server = s.spawn(|| {
            local::serve(daemon, &|req, progress| {
                let LocalRequest::Sync { opts } = req else {
                    panic!("{req:?}")
                };
                let r = crate::plan::apply::sync_with(&link, ctx, &opts, progress)
                    .map_err(|e| e.to_string())?;
                *report.lock().unwrap() = Some(r.clone());
                Ok(LocalReply::Synced(Box::new(r)))
            })
        });
        send(
            &mut cli,
            &LocalCall {
                version: LOCAL_WIRE_VERSION,
                request: LocalRequest::Sync {
                    opts: SyncOpts::default(),
                },
            },
        )
        .unwrap();
        let mut r = BufReader::new(&cli);
        loop {
            match recv_buffered::<_, LocalFrame>(&mut r).unwrap() {
                // Counters aside: the starts and ends are what is counted.
                LocalFrame::Progress(ProgressEvent::UnitProgress { .. }) => {}
                LocalFrame::Progress(e) => {
                    let beta =
                        matches!(&e, ProgressEvent::UnitStarted { rel, .. } if rel == "work/beta");
                    seen.push(e);
                    if beta {
                        break;
                    }
                }
                other => panic!("answered before work/beta began: {other:?}"),
            }
        }
        inj.wait_running(&link);
        // Ctrl-C: the CLI's process, and with it its end, is gone.
        drop(r);
        drop(cli);
        server.join().unwrap();
    });
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "stopped, not waited out: {:?}",
        started.elapsed()
    );
    inj.assert_stopped(&link);
    let r = report.into_inner().unwrap().expect("the sync reported");
    stopped_at_beta(&bed, &home, &r, HANGUP);
    let Outcome::Interrupted { why, .. } = &r.outcome else {
        unreachable!()
    };
    assert!(
        why.ends_with("transfer stopped") && !why.contains("lease"),
        "{why}"
    );
    // What the CLI saw before it left: work and alpha whole, beta begun.
    let started: Vec<&str> = seen
        .iter()
        .filter_map(|e| match e {
            ProgressEvent::UnitStarted { rel, .. } => Some(rel.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(started, ["work", "work/alpha", "work/beta"]);
    assert_eq!(seen.len(), 5, "{seen:#?}");
    assert!(home.queue.view().is_empty(), "the entry was given back");
    resumes(&bed, &home);
}

/// Review P2: the CLI hanging up after a unit's transfer finished whole
/// does not undo it. No exclusivity was lost, so the unit finishes
/// (baselines, marker cleared, journaled ok) and the sync stops before the
/// next one. The hangup lands as beta's transfer is done and verified and
/// the leaf tells home they agree.
#[test]
fn a_hangup_after_the_transfer_finished_lets_the_unit_finish() {
    let bed = Bed::enrolled();
    tree(&bed.home_hd.join("work"));
    let home = bed.home();
    let landed = std::sync::atomic::AtomicBool::new(false);
    let abort = crate::abort::Abort::default();
    let inner = InProcess {
        home: &home,
        host: "leaf".into(),
        home_dir: bed.leaf_hd.clone(),
    };
    let link = Faulty::new(inner, Fault::None).on_call(Box::new(|r, _| {
        if matches!(r, HomeRequest::Agree { rel, .. } if rel == "work/beta") {
            landed.store(true, std::sync::atomic::Ordering::SeqCst);
            abort.raise_because(HANGUP);
        }
        Ok(())
    }));
    let cfg = bed.leaf_cfg();
    let dirs = bed.leaf_dirs.state();
    let ctx = ctx(&bed, &cfg, &dirs);
    let emit = |_: ProgressEvent| {};
    let progress = Progress::new(&emit, abort.clone());
    let r = crate::plan::apply::sync_with(&link, ctx, &SyncOpts::default(), &progress).unwrap();
    assert!(
        landed.load(std::sync::atomic::Ordering::SeqCst),
        "beta's transfer never finished"
    );
    assert!(abort.raised());
    match &r.outcome {
        Outcome::Failed { rel, why } => {
            assert_eq!(rel, "work/nested/gamma");
            assert!(why.contains(HANGUP), "{why}");
            assert!(why.contains("stopped before this unit began"), "{why}");
        }
        o => panic!("{o:?}"),
    }
    let rels: Vec<&str> = r.applied.iter().map(|a| a.rel.as_str()).collect();
    assert_eq!(rels, ["work", "work/alpha", "work/beta"]);
    assert_eq!(
        std::fs::read_to_string(bed.leaf_hd.join("work/beta/README")).unwrap(),
        std::fs::read_to_string(bed.home_hd.join("work/beta/README")).unwrap()
    );
    let markers: Vec<Marker> = crate::marker::store(&bed.leaf_dirs.state()).all().unwrap();
    assert!(markers.is_empty(), "beta's marker was kept: {markers:?}");
    assert!(
        crate::baseline::store(&bed.leaf_dirs.state())
            .get::<Baseline>("work/beta")
            .unwrap()
            .is_some()
    );
    assert!(
        leaf_baselines(&bed.home_dirs.state(), "leaf")
            .get::<Baseline>("work/beta")
            .unwrap()
            .is_some()
    );
    let log = home.log(&Default::default()).unwrap();
    let beta: Vec<_> = log.iter().filter(|l| l.path == "work/beta").collect();
    assert_eq!(beta.len(), 1, "{beta:#?}");
    assert!(
        beta[0].detail.ends_with(" ok"),
        "settled: {}",
        beta[0].detail
    );
    assert!(!bed.leaf_hd.join("work/nested/gamma").exists());
    assert!(home.queue.view().is_empty(), "the entry was given back");
    // Nothing to resume: the next sync only brings gamma.
    let r = yes(&bed, &home);
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    let rels: Vec<&str> = r.applied.iter().map(|a| a.rel.as_str()).collect();
    assert_eq!(rels, ["work/nested/gamma"]);
}
