//! Review N1: the CLI hanging up mid-unit stops that unit's rsync, as a
//! lost lease does. The daemon side is the real `local::serve`; the CLI is
//! the other end of a socket pair, reading frames until `work/beta` starts
//! and its rsync (a script that records its pid and a grandchild's and
//! sleeps) is running, then closing.

use super::fail::{resumes, stopped_at_beta, wrapper};
use super::*;
use crate::daemon::local::{self, HANGUP};
use crate::daemon::remote::{recv_buffered, send};
use crate::daemon::wire::{LOCAL_WIRE_VERSION, LocalCall, LocalFrame, LocalReply, LocalRequest};
use crate::plan::tests::lease_kill::{gone, wait_for_pid};
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[test]
fn the_cli_hanging_up_kills_the_unit_rsync_and_stops_the_sync() {
    let bed = Bed::enrolled();
    tree(&bed.home_hd.join("work"));
    let home = bed.home();
    let (pid, kid) = (bed.tmp.path().join("p"), bed.tmp.path().join("k"));
    let fake = wrapper(
        bed.tmp.path(),
        "/work/beta",
        &format!(
            "echo $$ > {}; sleep 30 & echo $! > {}; wait; exit 0",
            pid.display(),
            kid.display()
        ),
    );
    let link = InProcess {
        home: &home,
        host: "leaf".into(),
        home_dir: bed.leaf_hd.clone(),
    };
    let cfg = bed.leaf_cfg();
    let dirs = (bed.leaf_dirs.state(), bed.leaf_dirs.scratch());
    let ctx = ctx(&bed, &cfg, &dirs, &fake);
    let report = Mutex::new(None);
    let (mut cli, daemon) = UnixStream::pair().unwrap();
    let started = Instant::now();
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
        wait_for_pid(&pid);
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
    for f in [&pid, &kid] {
        let p = wait_for_pid(f);
        assert!(gone(p), "{} {p} survived the hangup", f.display());
    }
    let r = report.into_inner().unwrap().expect("the sync reported");
    stopped_at_beta(&bed, &home, &r, HANGUP);
    let Outcome::Interrupted { why, .. } = &r.outcome else {
        unreachable!()
    };
    assert!(
        why.ends_with("rsync stopped") && !why.contains("lease"),
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
