//! A unit's fresh dry run must be the plan that was confirmed, whichever way
//! it goes (review B1): what changes between the plan and the unit's turn is
//! never carried out unseen. The change arrives as the unit is announced,
//! after the whole plan was made and confirmed.

use super::progress_tests::sync_watched;
use super::*;
use crate::testutil::fingerprint;

/// A clean first sync, then `edit` on one side; the next sync runs `hook` as
/// `work` is announced, and must fail it: "changed", untouched `side`, no
/// marker, no baseline moved.
fn stopped_by(
    edit: &str,
    hook: &(dyn Fn(&Bed) + Sync),
    side: &(dyn Fn(&Bed) -> std::path::PathBuf + Sync),
) {
    let bed = Bed::enrolled();
    tree(&bed.home_hd.join("work"));
    let home = bed.home();
    assert_eq!(yes(&bed, &home).outcome, Outcome::Done);
    testutil::next_second();
    testutil::write(&bed.home_hd.join(edit), "an edit\n");
    let before = std::sync::Mutex::new(None);
    let on = |e: &ProgressEvent, _: &crate::abort::Abort| {
        if let ProgressEvent::UnitStarted { rel, .. } = e
            && rel == "work"
            && before.lock().unwrap().is_none()
        {
            hook(&bed);
            *before.lock().unwrap() = Some(fingerprint(&side(&bed)));
        }
    };
    let (r, _) = sync_watched(&bed, &home, &on);
    match &r.outcome {
        Outcome::Failed { rel, why } => {
            assert_eq!(rel, "work");
            assert!(why.contains("changed since the plan"), "{why}");
        }
        o => panic!("not failed: {o:?}"),
    }
    assert_eq!(
        fingerprint(&side(&bed)),
        before.into_inner().unwrap().expect("the hook ran"),
        "nothing of it was written"
    );
    let m: Vec<Marker> = crate::marker::store(&bed.leaf_dirs.state()).all().unwrap();
    assert!(m.is_empty(), "stopped before the marker: {m:?}");
}

#[test]
fn a_pull_stops_at_leaf_work_made_after_the_plan() {
    // The review's g1: a new file, a new repo and an edit, as the unit starts.
    let hook = |bed: &Bed| {
        let leaf = bed.leaf_hd.join("work");
        testutil::write(&leaf.join("new-on-leaf.txt"), "leaf-only work\n");
        testutil::repo(&leaf.join("data/new-repo"));
        testutil::write(&leaf.join("data/blob.bin"), "edited on the leaf");
    };
    stopped_by("work/notes.txt", &hook, &|b: &Bed| b.leaf_hd.join("work"));
}

#[test]
fn a_pull_stops_at_a_home_change_made_after_the_plan() {
    let hook = |bed: &Bed| testutil::write(&bed.home_hd.join("work/late.txt"), "not planned\n");
    stopped_by("work/notes.txt", &hook, &|b: &Bed| b.leaf_hd.join("work"));
}

#[test]
fn a_confirmed_deletion_stops_at_a_file_added_after_the_plan() {
    // Home deleted the repo; the leaf's copy goes, as confirmed, unless more
    // arrived in it than the person who confirmed was shown.
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/r"));
    testutil::write(&bed.home_hd.join("work/keep.txt"), "x\n");
    let home = bed.home();
    assert_eq!(yes(&bed, &home).outcome, Outcome::Done);
    std::fs::remove_dir_all(bed.home_hd.join("work/r")).unwrap();
    testutil::next_second();
    let on = |e: &ProgressEvent, _: &crate::abort::Abort| {
        if let ProgressEvent::UnitStarted { rel, .. } = e
            && rel == "work/r"
        {
            testutil::write(&bed.leaf_hd.join("work/r/late.txt"), "leaf-only work\n");
        }
    };
    let opts = SyncOpts {
        confirm_delete: vec!["work/r".into()],
        ..Default::default()
    };
    let (r, _) = super::progress_tests::sync_watched_with(&bed, &home, &opts, &on);
    assert!(
        matches!(&r.outcome, Outcome::Failed { why, .. } if why.contains("changed since the plan")),
        "{:?}",
        r.outcome
    );
    assert!(bed.leaf_hd.join("work/r/late.txt").exists());
    assert!(
        bed.leaf_hd.join("work/r/.git/HEAD").exists(),
        "the repo stays"
    );
}

#[test]
fn a_pull_that_deletes_stops_at_a_file_added_after_the_plan() {
    let bed = Bed::enrolled();
    testutil::write(&bed.home_hd.join("work/only/a.txt"), "x\n");
    testutil::write(&bed.home_hd.join("work/keep.txt"), "x\n");
    let home = bed.home();
    assert_eq!(yes(&bed, &home).outcome, Outcome::Done);
    testutil::next_second();
    std::fs::remove_file(bed.home_hd.join("work/only/a.txt")).unwrap();
    let on = |e: &ProgressEvent, _: &crate::abort::Abort| {
        if let ProgressEvent::UnitStarted { rel, .. } = e
            && rel == "work"
        {
            testutil::write(&bed.leaf_hd.join("work/only/late.txt"), "leaf-only work\n");
        }
    };
    let (r, _) = sync_watched(&bed, &home, &on);
    assert!(
        matches!(&r.outcome, Outcome::Failed { why, .. } if why.contains("changed since the plan")),
        "{:?}",
        r.outcome
    );
    assert!(bed.leaf_hd.join("work/only/late.txt").exists());
    assert!(bed.leaf_hd.join("work/only/a.txt").exists());
}

#[test]
fn a_resumed_pull_that_changed_keeps_its_marker_and_writes_nothing() {
    let (bed, home) = super::marker_tests::interrupted("work", "/work/", "work/notes.txt");
    let on = |e: &ProgressEvent, _: &crate::abort::Abort| {
        if let ProgressEvent::UnitStarted { rel, .. } = e
            && rel == "work"
        {
            testutil::write(&bed.leaf_hd.join("work/late.txt"), "leaf-only work\n");
        }
    };
    let (r, _) = sync_watched(&bed, &home, &on);
    assert!(
        matches!(&r.outcome, Outcome::Failed { why, .. } if why.contains("changed since the plan")),
        "{:?}",
        r.outcome
    );
    assert!(bed.leaf_hd.join("work/late.txt").exists());
    let m: Vec<Marker> = crate::marker::store(&bed.leaf_dirs.state()).all().unwrap();
    assert_eq!(m.len(), 1, "the marker it was resuming stays");
}

#[test]
fn a_pull_nothing_moved_under_goes_ahead() {
    let bed = Bed::enrolled();
    tree(&bed.home_hd.join("work"));
    let home = bed.home();
    assert_eq!(yes(&bed, &home).outcome, Outcome::Done);
    testutil::next_second();
    testutil::write(&bed.home_hd.join("work/notes.txt"), "an edit\n");
    assert_eq!(yes(&bed, &home).outcome, Outcome::Done);
    assert_eq!(
        std::fs::read(bed.leaf_hd.join("work/notes.txt")).unwrap(),
        b"an edit\n"
    );
}

mod specials {
    use super::*;
    use crate::native::tests_fixture::bounded;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::time::Duration;

    fn fifo(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let c = CString::new(p.as_os_str().as_bytes()).unwrap();
        // SAFETY: `c` is a valid path for the call.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0);
    }

    /// The whole sync: every unit done, later ones too, the FIFO in the loose
    /// unit and the one in a repo's `.git` (as `core.fsmonitor` keeps a socket
    /// there) neither moved nor stopped anything. The review's p04, p05.
    #[test]
    fn a_sync_goes_around_specials_and_notes_them() {
        bounded(Duration::from_secs(60), || {
            let bed = Bed::enrolled();
            tree(&bed.home_hd.join("work"));
            fifo(&bed.home_hd.join("work/data/f"));
            fifo(&bed.home_hd.join("work/alpha/.git/fsmonitor"));
            let home = bed.home();
            let r = yes(&bed, &home);
            assert_eq!(r.outcome, Outcome::Done, "{:#?}", r.plan);
            assert!(bed.leaf_hd.join("work/nested/gamma/.git/HEAD").exists());
            assert!(!bed.leaf_hd.join("work/data/f").exists());
            assert!(!bed.leaf_hd.join("work/alpha/.git/fsmonitor").exists());
            let notes: Vec<&String> = r
                .plan
                .units
                .iter()
                .filter_map(|u| u.diff.as_ref())
                .flat_map(|d| &d.notes)
                .collect();
            assert_eq!(notes.len(), 2, "{notes:?}");
            assert!(notes.iter().any(|n| n.contains("data/f")));
            assert!(notes.iter().any(|n| n.contains(".git/fsmonitor")));
            // Nothing to do next time: the special is no difference.
            let again = plan_of(&bed, &home, true);
            assert!(
                again
                    .units
                    .iter()
                    .all(|u| matches!(u.decision, Decision::Nothing { .. })),
                "{:#?}",
                again.units
            );
        });
    }
}
