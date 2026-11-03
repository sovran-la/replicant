//! `sync`'s progress through the real daemons: the CLI's request over the
//! Unix socket, the leaf daemon applying against home over mTLS with the
//! native transfer, and each unit's start and end arriving on the socket
//! before the report, in plan order.

use super::test_certs::pair;
use super::*;
use crate::plan::apply::{Outcome, ProgressEvent, SyncOpts, UnitOutcome};
use crate::testutil::{self, Bed};

#[test]
fn a_sync_streams_each_unit_before_its_report() {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/r"));
    testutil::repo(&bed.home_hd.join("work/s"));
    testutil::write(&bed.home_hd.join("work/notes.txt"), "loose\n");
    let (_home, _leaf) = pair(&bed);
    let mut seen = Vec::new();
    let reply = local::call_streaming(
        &bed.leaf_dirs,
        LocalRequest::Sync {
            opts: SyncOpts::default(),
        },
        &mut |e| seen.push(e),
    )
    .unwrap();
    let LocalReply::Synced(r) = reply else {
        panic!("{reply:?}")
    };
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    let rels: Vec<&str> = r.applied.iter().map(|a| a.rel.as_str()).collect();
    assert_eq!(rels, ["work", "work/r", "work/s"]);
    // A unit's live counters come between its own start and end.
    let mut running = None;
    seen.retain(|e| match e {
        ProgressEvent::UnitStarted { rel, .. } => {
            running = Some(rel.clone());
            true
        }
        ProgressEvent::UnitProgress {
            rel,
            files_done,
            files_total,
            ..
        } => {
            assert_eq!(Some(rel), running.as_ref());
            assert!(files_done <= files_total);
            false
        }
        ProgressEvent::UnitDone { .. } => true,
    });
    assert_eq!(seen.len(), 2 * rels.len(), "{seen:#?}");
    for (i, (pair, a)) in seen.chunks(2).zip(&r.applied).enumerate() {
        assert_eq!(
            pair[0],
            ProgressEvent::UnitStarted {
                rel: a.rel.clone(),
                action: "pull".into(),
                direction: Some(crate::marker::Direction::Pull),
                index: i + 1,
                total: 3,
            }
        );
        let ProgressEvent::UnitDone {
            rel,
            files_changed,
            elapsed_ms,
            outcome,
            ..
        } = &pair[1]
        else {
            panic!("{:?}", pair[1])
        };
        assert_eq!((rel, *files_changed), (&a.rel, a.files_changed));
        assert_eq!(*outcome, UnitOutcome::Applied);
        assert_eq!(*elapsed_ms, a.elapsed_ms);
        assert!(*elapsed_ms <= r.elapsed_ms);
    }
    assert!(r.elapsed_ms > 0);

    // A dry run and a status announce nothing.
    for req in [
        LocalRequest::DryRun {
            no_wait: false,
            accept_rollback: vec![],
        },
        LocalRequest::Status,
    ] {
        local::call_streaming(&bed.leaf_dirs, req, &mut |e| panic!("announced {e:?}")).unwrap();
    }
}
