//! `sync` through the real daemons on loopback: the CLI's request reaches
//! the leaf daemon, which plans and applies against home over mTLS with the
//! system rsync, and both journals keep the lines. And an interrupted pull
//! as `status` shows it: an open marker with its age, a `resume` row, a dry
//! run that writes nothing, and the sync that finishes it.

use super::test_certs::pair;
use super::*;
use crate::baseline::Baseline;
use crate::decide::Decision;
use crate::marker::{Direction, Marker};
use crate::plan::apply::{Outcome, SyncOpts};
use crate::record::RECORD_VERSION;
use crate::state::UnitState;
use crate::testutil::{self, Bed};

fn local(dirs: &Dirs, req: LocalRequest) -> LocalReply {
    local::call(dirs, req).unwrap_or_else(|e| panic!("{e}"))
}

fn sync(bed: &Bed) -> Box<crate::plan::apply::SyncReport> {
    let LocalReply::Synced(r) = local(
        &bed.leaf_dirs,
        LocalRequest::Sync {
            opts: SyncOpts::default(),
        },
    ) else {
        panic!()
    };
    r
}

fn status(bed: &Bed) -> Box<crate::plan::LeafStatus> {
    let LocalReply::LeafStatus(s) = local(&bed.leaf_dirs, LocalRequest::Status) else {
        panic!()
    };
    s
}

#[test]
fn sync_applies_over_mtls_and_both_journals_keep_the_lines() {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/r"));
    let (home, leaf) = pair(&bed);
    let r = sync(&bed);
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    assert!(bed.leaf_hd.join("work/r/README").exists());
    let b: Option<Baseline> = crate::baseline::store(&bed.leaf_dirs.state())
        .get("work/r")
        .unwrap();
    assert!(b.is_some());
    for dirs in [&bed.leaf_dirs, &bed.home_dirs] {
        let LocalReply::Log { lines } = local(
            dirs,
            LocalRequest::Log {
                filter: Default::default(),
            },
        ) else {
            panic!()
        };
        assert!(
            lines
                .iter()
                .any(|l| l.event == "sync" && l.path == "work/r" && l.detail.starts_with("pull")),
            "{lines:#?}"
        );
    }
    let err = local::call(
        &bed.home_dirs,
        LocalRequest::Sync {
            opts: SyncOpts::default(),
        },
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("this machine is home"), "{err}");
    leaf.stop();
    home.stop();
}

#[test]
fn an_interrupted_pull_shows_in_status_resumes_and_clears() {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/r"));
    let (home, leaf) = pair(&bed);
    // A pull that died two hours ago, half done.
    testutil::write(&bed.leaf_hd.join("work/r/README"), "half\n");
    crate::marker::store(&bed.leaf_dirs.state())
        .put(&Marker {
            version: RECORD_VERSION,
            rel: "work/r".into(),
            direction: Direction::Pull,
            host: "leaf".into(),
            source_state: UnitState::Present("home-then".into()),
            started_at: crate::now_secs() - 7200,
        })
        .unwrap();
    let st = status(&bed);
    assert_eq!(st.markers.len(), 1, "{:#?}", st.markers);
    assert!(st.markers[0].alert && st.markers[0].age_secs >= 7200);
    assert_eq!(st.markers[0].on, "leaf");
    let text = crate::cli::render::leaf_status(&st, crate::cli::render::Style { color: false });
    assert!(text.contains("open markers:"), "{text}");
    assert!(text.contains("started 2h ago"), "{text}");
    let row = |p: &crate::plan::Plan| {
        p.units
            .iter()
            .find(|u| u.rel == "work/r")
            .unwrap()
            .decision
            .clone()
    };
    let resume = Decision::Resume {
        direction: Direction::Pull,
    };
    assert_eq!(row(st.plan.as_ref().unwrap()), resume);

    let before = testutil::fingerprint(&bed.leaf_hd);
    let LocalReply::Plan(p) = local(&bed.leaf_dirs, LocalRequest::DryRun { no_wait: true }) else {
        panic!()
    };
    assert_eq!(row(&p), resume);
    assert_eq!(
        testutil::fingerprint(&bed.leaf_hd),
        before,
        "the dry run writes nothing"
    );

    let r = sync(&bed);
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    assert!(r.applied.iter().any(|a| a.rel == "work/r" && a.resumed));
    assert_eq!(
        std::fs::read_to_string(bed.leaf_hd.join("work/r/README")).unwrap(),
        "hello\n"
    );
    let st = status(&bed);
    assert!(st.markers.is_empty(), "cleared");
    assert_eq!(
        row(st.plan.as_ref().unwrap()),
        Decision::Nothing {
            why: "unchanged".into()
        }
    );
    leaf.stop();
    home.stop();
}
