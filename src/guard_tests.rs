use super::*;
use crate::exclude::Excludes;
use crate::rsync::{self, Transfer};
use crate::state::UnitState;
use crate::testutil;
use crate::units::{Discovered, SubSpec, snapshot};
use std::path::Path;

fn unit(rel: &str) -> SideUnit {
    SideUnit {
        rel: rel.into(),
        sub: "work".into(),
        kind: UnitKind::Repo,
        state: UnitState::Present("x".into()),
        head: None,
        dirty: vec![],
        locks: vec![],
        collisions: vec![],
        is_file: false,
        files: 0,
        ignored: vec![],
    }
}

fn units_at(base: &Path, rel: &str, case_check: bool) -> Vec<SideUnit> {
    let subs = [SubSpec {
        rel: rel.into(),
        exclude: vec![],
        except: vec![],
        root: String::new(),
    }];
    let b = base.to_path_buf();
    let found = Discovered::run(&subs, &move |r| b.join(r)).unwrap();
    snapshot(&found, &found.repos(), case_check).unwrap()
}

// ---- guardrail 1 ----

#[test]
fn g1_dirty_target_file_that_rsync_would_overwrite_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home/work/r");
    let leaf = tmp.path().join("leaf/work/r");
    testutil::repo(&home);
    testutil::git(
        &home,
        &["commit", "-q", "--allow-empty", "-m", "home moved"],
    );
    std::fs::create_dir_all(leaf.parent().unwrap()).unwrap();
    let ok = std::process::Command::new("cp")
        .arg("-Rp")
        .arg(&home)
        .arg(&leaf)
        .status()
        .unwrap();
    assert!(ok.success());
    testutil::write(&home.join("README"), "home's newer README\n");
    testutil::git(&home, &["commit", "-qam", "edit"]);
    testutil::write(&leaf.join("README"), "leaf's uncommitted work\n");
    testutil::write(&leaf.join("scratch.txt"), "untracked, not on home\n");
    testutil::write(&leaf.join("café.txt"), "non-ASCII, untracked\n");

    let leaf_units = units_at(&tmp.path().join("leaf"), "work/r", false);
    let diff = Transfer {
        direction: Direction::Pull,
        leaf_path: leaf.clone(),
        home_path: home.clone(),
        home_host: None,
        is_file: false,
        excludes: Excludes::for_root(&[]).unwrap(),
        source_absent: false,
    }
    .dry_run(&testutil::runner())
    .unwrap();
    let r = dirty_target(&leaf_units[0], &diff).expect("refused");
    assert_eq!(r.guard, Guard::DirtyTarget);
    assert!(r.detail.contains(&"README".to_string()), "{r:?}");
    assert!(
        r.detail.contains(&"scratch.txt".to_string()),
        "--delete would remove it: {r:?}"
    );
    assert!(
        r.detail.contains(&"café.txt".to_string()),
        "a non-ASCII name matches git's raw UTF-8: {r:?}"
    );
    assert!(r.headline().contains("guardrail 1"));
}

#[test]
fn g1_dirty_path_rsync_leaves_alone_is_not_refused() {
    let mut u = unit("work/r");
    u.dirty = vec!["notes.md".into()];
    let diff = rsync::parse(">f.s..... other.rs\n");
    assert_eq!(dirty_target(&u, &diff), None);
}

#[test]
fn g1_clean_target_and_loose_units_pass() {
    let diff = rsync::parse(">f.s..... a\n*deleting b\n");
    assert_eq!(dirty_target(&unit("work/r"), &diff), None);
    let mut loose = unit("work");
    loose.kind = UnitKind::Loose;
    loose.dirty = vec!["a".into()];
    assert_eq!(
        dirty_target(&loose, &diff),
        None,
        "no git, no dirty concept"
    );
}

#[test]
fn g1_untracked_directory_entry_covers_files_beneath_it() {
    let mut u = unit("work/r");
    u.dirty = vec!["build-notes/".into()];
    let diff = rsync::parse("*deleting build-notes/a.txt\n");
    assert!(dirty_target(&u, &diff).is_some());
}

// ---- guardrail 3 ----

#[test]
fn g3_held_index_lock_on_source_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    testutil::tree(&tmp.path().join("work"));
    let units = units_at(tmp.path(), "work", false);
    let beta = units.iter().find(|u| u.rel == "work/beta").unwrap();
    let r = source_mid_write(beta).expect("refused");
    assert_eq!(r.guard, Guard::SourceMidWrite);
    assert_eq!(r.detail, vec![".git/index.lock"]);
    let alpha = units.iter().find(|u| u.rel == "work/alpha").unwrap();
    assert_eq!(source_mid_write(alpha), None);
}

// ---- guardrail 4 ----

#[test]
fn g4_collisions_refused_only_on_pull_to_insensitive_leaf() {
    let mut home = unit("work/r");
    home.collisions = vec![("README".into(), "readme".into())];
    let r = case_collision(&home, Direction::Pull, true).expect("refused");
    assert_eq!(r.guard, Guard::CaseCollision);
    assert!(r.detail[0].contains("README") && r.detail[0].contains("readme"));
    assert_eq!(
        case_collision(&home, Direction::Push, true),
        None,
        "push is not affected"
    );
    assert_eq!(
        case_collision(&home, Direction::Pull, false),
        None,
        "sensitive leaf"
    );
    assert_eq!(
        case_collision(&unit("work/r"), Direction::Pull, true),
        None,
        "no collisions"
    );
}

// ---- guardrail 5 ----

#[test]
fn g5_bounds_subscription_except_and_cap() {
    let subs = vec!["work/tundra".to_string(), "work/sequoia".to_string()];
    let cap = vec!["work/tundra".to_string(), "work/sequoia".to_string()];
    assert_eq!(bounds("work/tundra/beacon", &subs, &[], Some(&cap)), None);
    let r = bounds("work/rust", &subs, &[], Some(&cap)).expect("refused");
    assert_eq!(r.guard, Guard::SubscriptionBounds);
    assert_eq!(r.detail.len(), 2, "outside subscriptions and cap: {r:?}");
    let r = bounds("work/tundra/x", &subs, &["work/tundra/x".into()], None).expect("refused");
    assert!(r.detail[0].contains("unsubscribed"));
    // A cap narrowed after subscribing still bounds the old subscription.
    let narrowed = vec!["work/sequoia".to_string()];
    let r = bounds("work/tundra", &subs, &[], Some(&narrowed)).expect("refused");
    assert!(r.detail[0].contains("cap"), "{r:?}");
    assert!(
        bounds("work2", &["work".into()], &[], None).is_some(),
        "component-wise"
    );
}

#[test]
fn g5_diff_paths_must_stay_inside_the_unit() {
    assert_eq!(
        diff_bounds("work/r", &rsync::parse(">f+++++++ a/b\n")),
        None
    );
    let r = diff_bounds("work/r", &rsync::parse(">f+++++++ ../evil\n")).expect("refused");
    assert!(r.detail[0].contains("../evil"));
    assert!(diff_bounds("work/r", &rsync::parse(">f+++++++ /etc/x\n")).is_some());
}

#[test]
fn guard_numbers_match_the_plan() {
    let n: Vec<u8> = [
        Guard::DirtyTarget,
        Guard::SourceMidWrite,
        Guard::CaseCollision,
        Guard::SubscriptionBounds,
    ]
    .iter()
    .map(|g| g.number())
    .collect();
    assert_eq!(n, vec![1, 3, 4, 5]);
}

#[test]
fn guardrail_6_names_each_path_and_when_the_sync_began() {
    assert_eq!(changed_since_interrupted("work", 0, vec![]), None);
    let r = changed_since_interrupted("work", 1_762_000_000, vec!["a.md".into(), "d/b".into()])
        .unwrap();
    assert_eq!(r.guard, Guard::ChangedSinceInterrupted);
    assert_eq!(
        r.headline(),
        "guardrail 6 (changed here since the interrupted sync): work"
    );
    assert_eq!(r.detail.len(), 2);
    assert!(
        r.detail[0].starts_with("a.md: changed here after the interrupted sync began (2025-11-01T")
    );
    assert!(r.detail[1].starts_with("d/b: "));
}
