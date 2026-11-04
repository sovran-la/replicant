//! `sync` through the real daemons on loopback: the CLI's request reaches
//! the leaf daemon, which plans and applies against home over mTLS with the
//! native transfer, and both journals keep the lines. And an interrupted pull
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

/// The whole sync, control calls and data connections alike, over a leaf
/// whose first listed address is dead: everything falls through to the one
/// that answers, and the file lands.
#[test]
fn sync_falls_through_a_dead_listed_address_end_to_end() {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/r"));
    let home = Daemon::start(bed.home_dirs.clone()).unwrap();
    let live = home.tcp_addr().unwrap().to_string();
    let dead = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .to_string();
    super::test_certs::equip_leaf(&bed);
    super::test_certs::leaf_toml(&bed, &dead);
    let mut cfg = crate::config::LeafConfig::load(&bed.leaf_dirs.leaf())
        .unwrap()
        .unwrap();
    cfg.home.address = crate::config::Addresses::new(vec![dead, live]).unwrap();
    cfg.save(&bed.leaf_dirs.leaf()).unwrap();
    let leaf = Daemon::start(bed.leaf_dirs.clone()).unwrap();

    let r = sync(&bed);
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    assert!(bed.leaf_hd.join("work/r/README").exists());
    leaf.stop();
    home.stop();
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

    // The fleet view keeps the last sync past a later status, and shows the
    // leaf's open marker as of its last report.
    crate::marker::store(&bed.leaf_dirs.state())
        .put(&Marker {
            version: RECORD_VERSION,
            rel: "work/r".into(),
            direction: Direction::Pull,
            host: "leaf".into(),
            source_state: UnitState::Present("h".into()),
            started_at: crate::now_secs(),
            push: None,
        })
        .unwrap();
    status(&bed);
    let LocalReply::Fleet(f) = local(&bed.home_dirs, LocalRequest::Fleet) else {
        panic!()
    };
    let row = f.hosts.iter().find(|h| h.name == "leaf").unwrap();
    let last = row.last_sync.as_ref().expect("the sync is remembered");
    assert!(last.ok && last.summary.ends_with("; ok"), "{last:?}");
    assert_eq!(row.report.as_ref().unwrap().event, "status");
    assert_eq!(row.markers.len(), 1);
    assert_eq!(row.markers[0].rel, "work/r");
    let text = crate::cli::render::fleet(&f, crate::cli::render::Style { color: false });
    assert!(text.contains("open marker: pull work/r on leaf"), "{text}");
    leaf.stop();
    home.stop();
}

#[test]
fn an_interrupted_pull_shows_in_status_resumes_and_clears() {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/r"));
    let (home, leaf) = pair(&bed);
    // A pull that died two hours ago, half done: the transfer had finished README
    // (home's bytes and, with `-a`, home's mtime) but not `.git`. The resume
    // matches README and only creates, so guardrail 6, which looks at what
    // it would replace or delete, has nothing to say (review A3). A file
    // here cannot be given a ctime from before the backdated marker, so the
    // fixture holds nothing the resume would replace (review R1).
    let home_readme = bed.home_hd.join("work/r/README");
    let half = bed.leaf_hd.join("work/r/README");
    testutil::write(&half, &std::fs::read_to_string(&home_readme).unwrap());
    std::fs::File::options()
        .write(true)
        .open(&half)
        .unwrap()
        .set_modified(std::fs::metadata(&home_readme).unwrap().modified().unwrap())
        .unwrap();
    crate::marker::store(&bed.leaf_dirs.state())
        .put(&Marker {
            version: RECORD_VERSION,
            rel: "work/r".into(),
            direction: Direction::Pull,
            host: "leaf".into(),
            source_state: UnitState::Present("home-then".into()),
            started_at: crate::now_secs() - 7200,
            push: None,
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
    let LocalReply::Plan(p) = local(
        &bed.leaf_dirs,
        LocalRequest::DryRun {
            no_wait: true,
            accept_rollback: vec![],
        },
    ) else {
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

#[test]
fn a_push_over_mtls_is_listed_and_rolled_back_from_home_s_cli() {
    let bed = Bed::enrolled();
    testutil::write(&bed.home_hd.join("work/notes.txt"), "home's\n");
    let (home, leaf) = pair(&bed);
    assert_eq!(sync(&bed).outcome, Outcome::Done);
    testutil::write(&bed.leaf_hd.join("work/notes.txt"), "the leaf's, longer\n");
    let r = sync(&bed);
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    let id = r.applied[0].push.clone().expect("a push has a backup");
    let LocalReply::Pushes { pushes } =
        local(&bed.home_dirs, LocalRequest::Rollback { push: None })
    else {
        panic!()
    };
    assert_eq!(pushes[0].id, id);
    let err = local::call(
        &bed.leaf_dirs,
        LocalRequest::Rollback {
            push: Some(id.clone()),
        },
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("rollback runs on home"), "{err}");
    let LocalReply::RolledBack(rb) = local(
        &bed.home_dirs,
        LocalRequest::Rollback {
            push: Some(id.clone()),
        },
    ) else {
        panic!()
    };
    assert_eq!((rb.id.as_str(), rb.restored), (id.as_str(), 1));
    assert_eq!(
        std::fs::read_to_string(bed.home_hd.join("work/notes.txt")).unwrap(),
        "home's\n"
    );
    let text = crate::cli::render::rolled_back(&rb);
    assert!(
        text.contains(&format!(
            "Rolled back {id} (work by leaf): restored 1 files"
        )),
        "{text}"
    );
    leaf.stop();
    home.stop();
}
