//! Rollback against real pushes in a `Bed`: what comes back, what goes,
//! what is refused; and the plan it follows, by itself.

use super::*;
use crate::daemon::home::Run;
use crate::plan::apply::tests::{plan_of, sync_with, tree, yes};
use crate::plan::apply::{Outcome, SyncOpts};
use crate::testutil::{self, Bed};
use std::collections::BTreeMap;

type Print = BTreeMap<String, (Vec<u8>, u32, u64)>;

/// Files and links with bytes, mode and mtime to the second (openrsync
/// keeps whole seconds); directories by mode only (their mtimes move with
/// their contents and are not restored).
fn print(root: &std::path::Path) -> Print {
    testutil::fingerprint(root)
        .into_iter()
        .map(|(k, (bytes, mode, at))| {
            let secs = at.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
            let secs = if k.ends_with('/') { 0 } else { secs };
            (k, (bytes, mode, secs))
        })
        .collect()
}

/// Home and leaf agree on the tree; then the leaf edits, deletes, adds,
/// commits and pushes. Returns home's print before the push and the ids.
fn pushed(bed: &Bed, home: &Home) -> (Print, Vec<String>) {
    tree(&bed.home_hd.join("work"));
    assert_eq!(yes(bed, home).outcome, Outcome::Done);
    let w = bed.leaf_hd.join("work");
    testutil::write(&w.join("notes.txt"), "edited on the leaf, longer\n");
    std::fs::remove_file(w.join("data/blob.bin")).unwrap();
    testutil::write(&w.join("data/new.bin"), "new");
    testutil::write(&w.join("newdir/deep/x"), "a new tree");
    testutil::write(&w.join("alpha/README"), "committed on the leaf\n");
    testutil::git(&w.join("alpha"), &["commit", "-qam", "leaf"]);
    let before = print(&bed.home_hd);
    let r = yes(bed, home);
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    let ids: Vec<String> = r.applied.iter().filter_map(|a| a.push.clone()).collect();
    assert_eq!(ids.len(), 2, "work and work/alpha: {:#?}", r.applied);
    assert_ne!(print(&bed.home_hd), before, "the push changed home");
    (before, ids)
}

#[test]
fn a_push_rolled_back_leaves_home_as_it_was() {
    let bed = Bed::enrolled();
    let home = bed.home();
    let (before, ids) = pushed(&bed, &home);
    let listed: Vec<String> = home.pushes().unwrap().into_iter().map(|p| p.id).collect();
    assert_eq!(listed.len(), 2);
    for id in &ids {
        let r = home.rollback(id).unwrap();
        assert!(r.unbacked.is_empty() && r.unreported.is_empty(), "{r:?}");
        assert!(r.kept.is_dir(), "what it undid is kept");
    }
    assert_eq!(print(&bed.home_hd), before);
    let log = home.log(&Default::default()).unwrap();
    let rb: Vec<&str> = log
        .iter()
        .filter(|l| l.event == "rollback")
        .map(|l| l.path.as_str())
        .collect();
    assert_eq!(rb.len(), 2, "{log:#?}");
    // The push's journal line named the backup it can be rolled back from.
    assert!(
        log.iter()
            .any(|l| l.event == "sync" && l.detail.contains(&format!("backup:{}", ids[0]))),
        "{log:#?}"
    );
    let again = home.rollback(&ids[0]).unwrap_err().to_string();
    assert!(again.contains("already rolled back"), "{again}");
    assert!(home.queue.view().is_empty());
}

#[test]
fn rollback_refuses_what_it_cannot_undo_safely() {
    let bed = Bed::enrolled();
    let home = bed.home();
    let (_, ids) = pushed(&bed, &home);
    for bad in [
        "../etc",
        "leaf",
        "leaf/now",
        "leaf/20261101T000000Z/../x",
        "",
    ] {
        let e = home.rollback(bad).unwrap_err().to_string();
        assert!(e.contains("not a push id"), "{bad}: {e}");
    }
    let e = home
        .rollback("leaf/20200101T000000Z")
        .unwrap_err()
        .to_string();
    assert!(e.contains("no push"), "{e}");

    // A later push of the same unit must be rolled back first.
    testutil::write(&bed.leaf_hd.join("work/notes.txt"), "a second push\n");
    let r = yes(&bed, &home);
    let second = r.applied[0].push.clone().unwrap();
    let loose = ids.iter().find(|i| {
        home.pushes()
            .unwrap()
            .iter()
            .any(|p| &p.id == *i && p.unit == "work")
    });
    let e = home.rollback(loose.unwrap()).unwrap_err().to_string();
    assert!(e.contains(&format!("roll back {second} first")), "{e}");
    home.rollback(&second).unwrap();
    home.rollback(loose.unwrap()).unwrap();

    // A swept backup cannot be restored from.
    let alpha = ids.iter().find(|i| *i != loose.unwrap()).unwrap();
    std::fs::remove_dir_all(home.push_dir(alpha)).unwrap();
    let e = home.rollback(alpha).unwrap_err().to_string();
    assert!(e.contains("backup is gone"), "{e}");
}

#[test]
fn an_interrupted_push_is_rolled_back_and_its_quarantine_ends() {
    let bed = Bed::enrolled();
    testutil::write(&bed.home_hd.join("work/notes.txt"), "home's\n");
    let home = bed.home();
    assert_eq!(yes(&bed, &home).outcome, Outcome::Done);
    testutil::write(&bed.leaf_hd.join("work/notes.txt"), "the leaf's, longer\n");
    testutil::write(&bed.leaf_hd.join("work/added.txt"), "new\n");
    let before = print(&bed.home_hd);
    // The real rsync runs, then the "transfer" fails: home was written.
    let fake =
        crate::plan::apply::tests::fail::wrapper(bed.tmp.path(), "/work/", "rsync \"$@\"; exit 23");
    let r = sync_with(&bed, &home, &SyncOpts::default(), &fake).unwrap();
    assert!(matches!(r.outcome, Outcome::Interrupted { .. }), "{r:#?}");
    assert_ne!(print(&bed.home_hd), before);
    let m: Vec<Marker> = marker::store(&bed.home_dirs.state()).all().unwrap();
    let id = m[0].push.clone().expect("the marker names its push");

    let rb = home.rollback(&id).unwrap();
    assert_eq!((rb.restored, rb.removed), (1, 1), "{rb:?}");
    assert_eq!(print(&bed.home_hd), before);
    assert!(
        marker::store(&bed.home_dirs.state())
            .all::<Marker>()
            .unwrap()
            .is_empty(),
        "rollback ends the quarantine"
    );
    let p = plan_of(&bed, &home, false);
    let d = &p.units.iter().find(|u| u.rel == "work").unwrap().decision;
    assert_eq!(
        d,
        &crate::decide::Decision::Push,
        "home is back at its baseline; the leaf still has its change, so it pushes again"
    );
}

#[test]
fn a_resume_backs_up_into_a_run_of_its_own() {
    let bed = Bed::enrolled();
    testutil::write(&bed.home_hd.join("work/notes.txt"), "original\n");
    let home = bed.home();
    assert_eq!(yes(&bed, &home).outcome, Outcome::Done);
    testutil::write(&bed.leaf_hd.join("work/notes.txt"), "first try, longer\n");
    let fake =
        crate::plan::apply::tests::fail::wrapper(bed.tmp.path(), "/work/", "rsync \"$@\"; exit 23");
    let r = sync_with(&bed, &home, &SyncOpts::default(), &fake).unwrap();
    assert!(matches!(r.outcome, Outcome::Interrupted { .. }));
    testutil::write(
        &bed.leaf_hd.join("work/notes.txt"),
        "second try, longer still\n",
    );
    let r = yes(&bed, &home);
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    let p = &home.pushes().unwrap()[0];
    assert_eq!(p.runs.len(), 2);
    let run = |n: usize| home.push_dir(&p.id).join(format!("run-{n}/notes.txt"));
    assert_eq!(std::fs::read_to_string(run(1)).unwrap(), "original\n");
    assert_eq!(
        std::fs::read_to_string(run(2)).unwrap(),
        "first try, longer\n"
    );
    home.rollback(&p.id).unwrap();
    assert_eq!(
        std::fs::read_to_string(bed.home_hd.join("work/notes.txt")).unwrap(),
        "original\n",
        "the first run's copy is the original"
    );
}

#[test]
fn a_single_file_unit_is_backed_up_and_rolled_back() {
    let bed = Bed::new("");
    let hd = bed.leaf_hd.display().to_string();
    let mut text = std::fs::read_to_string(bed.home_dirs.fleet()).unwrap();
    text.push_str(&format!(
        "[hosts.leaf]\nhome_dir = \"{hd}\"\n\n[subscriptions.leaf]\npaths = [\"work/settings.json\"]\n"
    ));
    std::fs::write(bed.home_dirs.fleet(), text).unwrap();
    testutil::write(&bed.home_hd.join("work/settings.json"), "{\"home\": 1}\n");
    testutil::write(&bed.home_hd.join("work/sibling"), "not in the unit\n");
    let home = bed.home();
    assert_eq!(yes(&bed, &home).outcome, Outcome::Done);
    testutil::write(&bed.leaf_hd.join("work/settings.json"), "{\"leaf\": 22}\n");
    let before = print(&bed.home_hd);
    let r = yes(&bed, &home);
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    let id = r.applied[0].push.clone().unwrap();
    home.rollback(&id).unwrap();
    assert_eq!(print(&bed.home_hd), before);
}

#[path = "home_backup_tests.rs"]
mod backup;
