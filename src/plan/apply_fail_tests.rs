//! What a failed or stopped unit leaves: its marker, no baseline for it, an
//! `interrupted` journal line, exit 1, and every later unit untouched. And
//! that the next dry run shows `resume` and writes nothing, and the next
//! sync resumes and clears the marker.

use super::*;
use crate::plan::tests::lease_kill::{gone, wait_for_pid};
use std::time::Duration;

/// rsync, except that a real run (no `--dry-run`) whose arguments mention
/// `pat` does `then` instead.
pub(crate) fn wrapper(dir: &Path, pat: &str, then: &str) -> String {
    let body = format!(
        "for a in \"$@\"; do [ \"$a\" = --dry-run ] && exec rsync \"$@\"; done\n\
         for a in \"$@\"; do case \"$a\" in *{pat}*) {then} ;; esac; done\n\
         exec rsync \"$@\""
    );
    testutil::script(dir, "rsync-wrapper", &body)
        .display()
        .to_string()
}

fn baseline(bed: &Bed, rel: &str) -> Option<Baseline> {
    crate::baseline::store(&bed.leaf_dirs.state())
        .get(rel)
        .unwrap()
}

/// After `work/beta` stopped: alpha done, beta marked and not baselined,
/// gamma not touched, and the journal says so.
fn stopped_at_beta(bed: &Bed, home: &Home, r: &SyncReport, why: &str) {
    match &r.outcome {
        Outcome::Interrupted {
            rel,
            why: w,
            journaled,
        } => {
            assert_eq!(rel, "work/beta");
            assert!(w.contains(why), "{w}");
            assert!(journaled);
        }
        o => panic!("not interrupted: {o:?}"),
    }
    assert_eq!(r.outcome.exit_code(), 1);
    let done: Vec<&str> = r.applied.iter().map(|a| a.rel.as_str()).collect();
    assert_eq!(done, ["work", "work/alpha"]);
    assert!(baseline(bed, "work/alpha").is_some());
    assert!(
        baseline(bed, "work/beta").is_none(),
        "no baseline after a failure"
    );
    assert!(baseline(bed, "work/nested/gamma").is_none());
    assert!(
        !bed.leaf_hd.join("work/nested/gamma").exists(),
        "a later unit is untouched"
    );
    let m: Vec<Marker> = crate::marker::store(&bed.leaf_dirs.state()).all().unwrap();
    assert_eq!(m.len(), 1, "{m:?}");
    assert_eq!(
        (m[0].rel.as_str(), m[0].host.as_str()),
        ("work/beta", "leaf")
    );
    let log = home.log(&Default::default()).unwrap();
    let line = log
        .iter()
        .find(|l| l.event == "sync" && l.path == "work/beta")
        .expect("journaled");
    assert!(line.detail.contains("interrupted"), "{}", line.detail);
    assert!(line.detail.contains(why), "{}", line.detail);
}

/// The next dry run says resume and writes nothing; the next sync resumes,
/// clears the marker and finishes the rest.
fn resumes(bed: &Bed, home: &Home) {
    let prints = (
        testutil::fingerprint(&bed.leaf_hd),
        testutil::fingerprint(&bed.leaf_dirs.state()),
    );
    let p = plan_of(bed, home, true);
    assert_eq!(
        unit(&p, "work/beta", UnitKind::Repo).decision,
        Decision::Resume {
            direction: crate::marker::Direction::Pull
        }
    );
    assert_eq!(
        prints,
        (
            testutil::fingerprint(&bed.leaf_hd),
            testutil::fingerprint(&bed.leaf_dirs.state())
        ),
        "a dry run after an interrupted apply writes nothing"
    );
    let r = yes(bed, home);
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    let beta = r.applied.iter().find(|a| a.rel == "work/beta").unwrap();
    assert!(beta.resumed);
    assert!(
        crate::marker::store(&bed.leaf_dirs.state())
            .all::<Marker>()
            .unwrap()
            .is_empty()
    );
    assert!(baseline(bed, "work/beta").is_some());
    assert!(bed.leaf_hd.join("work/nested/gamma/README").exists());
    let log = home.log(&Default::default()).unwrap();
    assert!(
        log.iter()
            .any(|l| l.path == "work/beta" && l.detail.ends_with("resumed → ok")),
        "{log:#?}"
    );
}

#[test]
fn an_rsync_that_fails_stops_the_sync_at_its_unit() {
    let bed = Bed::enrolled();
    tree(&bed.home_hd.join("work"));
    let home = bed.home();
    let fake = wrapper(
        bed.tmp.path(),
        "/work/beta",
        "echo '>f+++++++ README'; echo 'rsync: connection lost' >&2; exit 23",
    );
    let r = sync_with(&bed, &home, &SyncOpts::default(), &fake).unwrap();
    stopped_at_beta(&bed, &home, &r, "rsync exit 23");
    resumes(&bed, &home);
}

#[test]
fn a_killed_rsync_stops_the_sync_at_its_unit() {
    let bed = Bed::enrolled();
    tree(&bed.home_hd.join("work"));
    let home = bed.home();
    let pid = bed.tmp.path().join("rsync.pid");
    let fake = wrapper(
        bed.tmp.path(),
        "/work/beta",
        &format!("echo $$ > {}; sleep 30; exit 0", pid.display()),
    );
    let r = std::thread::scope(|s| {
        s.spawn(|| {
            let p = wait_for_pid(&pid);
            // SAFETY: signalling a test's own child script.
            unsafe { libc::kill(p, libc::SIGKILL) };
        });
        sync_with(&bed, &home, &SyncOpts::default(), &fake).unwrap()
    });
    stopped_at_beta(&bed, &home, &r, "killed by a signal");
    resumes(&bed, &home);
}

#[test]
fn a_lost_lease_kills_the_unit_rsync_and_stops_the_sync() {
    let bed = Bed::enrolled();
    testutil::hand_edit(
        &bed.home_dirs.fleet(),
        "listen = ",
        "lease_secs = 1\nlisten = ",
    );
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
    let started = std::time::Instant::now();
    let r = std::thread::scope(|s| {
        s.spawn(|| {
            wait_for_pid(&pid);
            let head = home.queue.view()[0].id;
            home.queue.release(head).unwrap();
        });
        sync_with(&bed, &home, &SyncOpts::default(), &fake).unwrap()
    });
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "stopped, not waited out"
    );
    for f in [&pid, &kid] {
        let p = wait_for_pid(f);
        assert!(gone(p), "{} {p} survived", f.display());
    }
    stopped_at_beta(&bed, &home, &r, "lease lost");
    resumes(&bed, &home);
}

#[test]
fn an_interrupted_push_quarantines_home_and_its_own_leaf_resumes_it() {
    let bed = Bed::enrolled();
    tree(&bed.leaf_hd.join("work"));
    let home = bed.home();
    let fake = wrapper(bed.tmp.path(), "/work/beta", "exit 12");
    let r = sync_with(&bed, &home, &SyncOpts::default(), &fake).unwrap();
    assert!(matches!(r.outcome, Outcome::Interrupted { ref rel, .. } if rel == "work/beta"));
    let m: Vec<Marker> = crate::marker::store(&bed.home_dirs.state()).all().unwrap();
    assert_eq!(m.len(), 1);
    assert_eq!(
        (m[0].rel.as_str(), m[0].host.as_str()),
        ("work/beta", "leaf")
    );
    assert!(
        crate::marker::store(&bed.leaf_dirs.state())
            .all::<Marker>()
            .unwrap()
            .is_empty(),
        "a push's marker is on home, its target"
    );
    let p = plan_of(&bed, &home, true);
    assert_eq!(
        unit(&p, "work/beta", UnitKind::Repo).decision,
        Decision::Resume {
            direction: crate::marker::Direction::Push
        }
    );
    let r = yes(&bed, &home);
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    assert!(
        crate::marker::store(&bed.home_dirs.state())
            .all::<Marker>()
            .unwrap()
            .is_empty()
    );
    assert!(baseline(&bed, "work/beta").is_some());
}
