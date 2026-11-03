//! Guardrail 6 (review A3): a resume carries a decision made before it was
//! interrupted, and the marker records only the source's state. What the
//! leaf has written since is not covered by it, so a resumed pull, removal
//! or confirmed deletion that would replace or delete such a path blocks
//! its unit until `--confirm-delete <unit>` says so.

use super::marker_tests::{fail_at, interrupted};
use super::*;
use crate::daemon::wire::HomeRequest;
use crate::guard::Guard;
use crate::marker::Direction;
use crate::plan::HomeLink;

fn guard6<'p>(p: &'p Plan, rel: &str, kind: UnitKind) -> &'p crate::guard::Refusal {
    let u = unit(p, rel, kind);
    u.refusals
        .iter()
        .find(|r| r.guard == Guard::ChangedSinceInterrupted)
        .unwrap_or_else(|| panic!("no guardrail 6 on {rel}: {u:#?}"))
}

fn commit(repo: &Path, file: &str) {
    testutil::git(repo, &["add", file]);
    testutil::git(repo, &["commit", "-qm", "on the leaf"]);
}

fn confirm(rel: &str) -> SyncOpts {
    SyncOpts {
        confirm_delete: vec![rel.into()],
        ..Default::default()
    }
}

fn blocked_on(r: &SyncReport, rel: &str) {
    let Outcome::Blocked { why } = &r.outcome else {
        panic!("not blocked: {:#?}", r.outcome);
    };
    assert_eq!(
        why,
        &[format!(
            "guardrail 6 (changed here since the interrupted sync): {rel}; pass --confirm-delete \
             {rel} to let the resume replace or delete them"
        )]
    );
    assert!(r.applied.is_empty());
}

#[test]
fn a_resumed_pull_will_not_delete_a_leaf_file_made_after_it_was_interrupted() {
    let (bed, home) = interrupted("work", "/work/", "work/notes.txt");
    let later = bed.leaf_hd.join("work/todo-made-later.md");
    testutil::write(&later, "written on the leaf after the interruption\n");

    let p = plan_of(&bed, &home, true);
    assert_eq!(
        unit(&p, "work", UnitKind::Loose).decision,
        Decision::Resume {
            direction: Direction::Pull
        }
    );
    let g = guard6(&p, "work", UnitKind::Loose);
    assert_eq!(
        g.detail.len(),
        1,
        "only the new file, not notes.txt: {g:#?}"
    );
    assert!(g.detail[0].starts_with("todo-made-later.md: changed here after"));

    let before = testutil::fingerprint(&bed.leaf_hd);
    let r = yes(&bed, &home);
    blocked_on(&r, "work");
    assert_eq!(
        testutil::fingerprint(&bed.leaf_hd),
        before,
        "nothing written"
    );
    // A confirmation for another unit does not waive it, and names nothing.
    let r = sync_with(&bed, &home, &confirm("work/alpha"), crate::plan::RSYNC).unwrap();
    assert!(matches!(r.outcome, Outcome::Blocked { .. }));
    assert!(later.exists());

    let r = sync_with(&bed, &home, &confirm("work"), crate::plan::RSYNC).unwrap();
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    assert!(!later.exists(), "confirmed: the resume deleted it");
    assert_eq!(
        std::fs::read_to_string(bed.leaf_hd.join("work/notes.txt")).unwrap(),
        "home edit\n"
    );
}

#[test]
fn a_resumed_pull_will_not_delete_a_file_moved_in_with_an_old_mtime() {
    // `mv` from ~/Downloads, `cp -p`, `tar x`: the mtime is a week old, but
    // the file arrived after the interruption (review R1).
    let (bed, home) = interrupted("work", "/work/", "work/notes.txt");
    let moved = bed.leaf_hd.join("work/moved-in.pdf");
    testutil::write(&moved, "only on the leaf\n");
    let week_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(7 * 86_400);
    std::fs::File::options()
        .write(true)
        .open(&moved)
        .unwrap()
        .set_modified(week_ago)
        .unwrap();
    assert!(std::fs::metadata(&moved).unwrap().modified().unwrap() <= week_ago);

    let p = plan_of(&bed, &home, true);
    let g = guard6(&p, "work", UnitKind::Loose);
    assert_eq!(g.detail.len(), 1, "{g:#?}");
    assert!(g.detail[0].starts_with("moved-in.pdf: changed here after"));
    let before = testutil::fingerprint(&bed.leaf_hd);
    blocked_on(&yes(&bed, &home), "work");
    assert_eq!(
        testutil::fingerprint(&bed.leaf_hd),
        before,
        "nothing written"
    );

    let r = sync_with(&bed, &home, &confirm("work"), crate::plan::RSYNC).unwrap();
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    assert!(!moved.exists(), "confirmed: the resume deleted it");
}

#[test]
fn a_resumed_repo_pull_will_not_undo_a_leaf_commit_made_after_it_was_interrupted() {
    // Guardrail 1 sees only uncommitted work; this commit is clean.
    let (bed, home) = interrupted("work/alpha", "/work/alpha/", "work/alpha/README");
    let alpha = bed.leaf_hd.join("work/alpha");
    testutil::write(&alpha.join("mine.txt"), "committed on the leaf\n");
    testutil::git(&alpha, &["add", "mine.txt"]);
    testutil::git(&alpha, &["commit", "-qm", "leaf"]);
    let p = plan_of(&bed, &home, true);
    let g = guard6(&p, "work/alpha", UnitKind::Repo);
    assert!(
        g.detail.iter().any(|d| d.starts_with("mine.txt: ")),
        "the working tree: {g:#?}"
    );
    assert!(
        g.detail.iter().any(|d| d.starts_with(".git/")),
        "and the refs the commit moved: {g:#?}"
    );
    assert!(
        !g.detail.iter().any(|d| d.starts_with("README: ")),
        "README was not touched here since: {g:#?}"
    );
    let before = testutil::fingerprint(&alpha);
    blocked_on(&yes(&bed, &home), "work/alpha");
    assert_eq!(testutil::fingerprint(&alpha), before);
}

#[test]
fn a_resumed_removal_will_not_delete_what_the_leaf_made_after_it_was_interrupted() {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/r"));
    let home = bed.home();
    assert_eq!(yes(&bed, &home).outcome, Outcome::Done);
    let link = InProcess {
        home: &home,
        host: "leaf".into(),
        home_dir: bed.leaf_hd.clone(),
    };
    link.call(HomeRequest::Unsubscribe {
        paths: vec!["work/r".into()],
    })
    .unwrap();
    // The marker starts after the first sync's writes (see `interrupted`).
    testutil::next_second();
    let r = sync_inject(
        &bed,
        &home,
        &SyncOpts::default(),
        &fail_at(&bed, "/work/r/").deleting(),
    )
    .unwrap();
    assert!(
        matches!(&r.outcome, Outcome::Interrupted { rel, .. } if rel == "work/r"),
        "{r:#?}"
    );

    // Committed, so guardrail 1 (uncommitted work) has nothing to say.
    let later = bed.leaf_hd.join("work/r/later.txt");
    testutil::write(
        &later,
        "made on the leaf after the removal was interrupted\n",
    );
    commit(&bed.leaf_hd.join("work/r"), "later.txt");
    let p = plan_of(&bed, &home, true);
    assert_eq!(
        unit(&p, "work/r", UnitKind::Repo).decision,
        Decision::Remove
    );
    let g = guard6(&p, "work/r", UnitKind::Repo);
    assert!(
        g.detail.iter().any(|d| d.starts_with("later.txt: ")),
        "{g:#?}"
    );
    assert!(
        !g.detail.iter().any(|d| d.starts_with("README: ")),
        "{g:#?}"
    );
    let before = testutil::fingerprint(&bed.leaf_hd);
    blocked_on(&yes(&bed, &home), "work/r");
    assert_eq!(testutil::fingerprint(&bed.leaf_hd), before);

    let r = sync_with(&bed, &home, &confirm("~/work/r"), crate::plan::RSYNC).unwrap();
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    assert!(!later.exists() && !bed.leaf_hd.join("work/r/README").exists());
}

#[test]
fn a_resumed_confirmed_deletion_does_not_reuse_its_confirmation_for_newer_files() {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/r"));
    let home = bed.home();
    assert_eq!(yes(&bed, &home).outcome, Outcome::Done);
    std::fs::remove_dir_all(bed.home_hd.join("work/r")).unwrap();
    let r = sync_inject(
        &bed,
        &home,
        &confirm("work/r"),
        &fail_at(&bed, "/work/r/").deleting(),
    )
    .unwrap();
    assert!(
        matches!(&r.outcome, Outcome::Interrupted { rel, .. } if rel == "work/r"),
        "{r:#?}"
    );

    let later = bed.leaf_hd.join("work/r/later.txt");
    testutil::write(&later, "new\n");
    commit(&bed.leaf_hd.join("work/r"), "later.txt");
    let p = plan_of(&bed, &home, true);
    assert_eq!(
        unit(&p, "work/r", UnitKind::Repo).decision,
        Decision::Resume {
            direction: Direction::Pull
        }
    );
    guard6(&p, "work/r", UnitKind::Repo);
    blocked_on(&yes(&bed, &home), "work/r");
    assert!(later.exists());
}

#[test]
fn a_resume_with_nothing_changed_here_since_is_not_held() {
    let (bed, home) = interrupted("work", "/work/", "work/notes.txt");
    let p = plan_of(&bed, &home, true);
    assert!(
        unit(&p, "work", UnitKind::Loose).refusals.is_empty(),
        "{p:#?}"
    );
    let r = yes(&bed, &home);
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    assert!(r.applied.iter().any(|a| a.rel == "work" && a.resumed));
}
