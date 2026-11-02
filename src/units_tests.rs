use super::*;
use crate::testutil;
use std::path::Path;

fn spec(rel: &str) -> SubSpec {
    SubSpec {
        rel: rel.into(),
        exclude: vec!["target".into()],
        except: vec![],
        root: String::new(),
    }
}

fn side(base: &Path, subs: &[SubSpec]) -> Discovered {
    let base = base.to_path_buf();
    Discovered::run(subs, &move |rel| base.join(rel)).unwrap()
}

fn find<'a>(units: &'a [SideUnit], rel: &str, kind: UnitKind) -> &'a SideUnit {
    units
        .iter()
        .find(|u| u.rel == rel && u.kind == kind)
        .unwrap_or_else(|| panic!("no {kind:?} unit {rel} in {units:#?}"))
}

#[test]
fn fixture_units_repos_plus_one_loose_per_subscription() {
    let tmp = tempfile::tempdir().unwrap();
    testutil::tree(&tmp.path().join("work"));
    let subs = [spec("work")];
    let found = side(tmp.path(), &subs);
    assert_eq!(
        found.repos(),
        vec!["work/alpha", "work/beta", "work/nested/gamma"]
    );
    assert_eq!(found.worktrees(), vec!["work/alpha-wt"]);
    let units = snapshot(&found, &found.repos(), false).unwrap();
    assert_eq!(units.len(), 4);
    let beta = find(&units, "work/beta", UnitKind::Repo);
    assert_eq!(beta.locks, vec![".git/index.lock"]);
    assert_eq!(beta.dirty, vec!["README", "new.txt"]);
    let loose = find(&units, "work", UnitKind::Loose);
    assert!(!loose.state.is_empty());
}

#[test]
fn union_makes_a_home_only_repo_an_empty_unit_on_the_leaf() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let leaf = tmp.path().join("leaf");
    testutil::repo(&home.join("work/only-home"));
    std::fs::create_dir_all(leaf.join("work")).unwrap();
    let subs = [spec("work")];
    let h = side(&home, &subs);
    let l = side(&leaf, &subs);
    let mut union = h.repos();
    union.extend(l.repos());
    let lu = snapshot(&l, &union, false).unwrap();
    assert_eq!(
        find(&lu, "work/only-home", UnitKind::Repo).state,
        UnitState::Empty
    );
    assert_eq!(find(&lu, "work", UnitKind::Loose).state, UnitState::Empty);
    let hu = snapshot(&h, &union, false).unwrap();
    assert!(!find(&hu, "work/only-home", UnitKind::Repo).state.is_empty());
    assert_eq!(
        find(&hu, "work", UnitKind::Loose).state,
        UnitState::Empty,
        "the only content is the repo, which is not loose"
    );
}

#[test]
fn plain_directory_where_other_side_has_repo_never_equals_the_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    testutil::repo(&a.join("work/x"));
    testutil::write(&b.join("work/x/README"), "hello\n");
    let subs = [spec("work")];
    let union = side(&a, &subs).repos();
    let au = snapshot(&side(&a, &subs), &union, false).unwrap();
    let bu = snapshot(&side(&b, &subs), &union, false).unwrap();
    let (ar, br) = (
        find(&au, "work/x", UnitKind::Repo),
        find(&bu, "work/x", UnitKind::Repo),
    );
    assert!(!br.state.is_empty());
    assert_ne!(ar.state, br.state);
    assert!(br.head.is_none());
    // And the plain dir's contents are not double-counted in b's loose unit.
    assert_eq!(find(&bu, "work", UnitKind::Loose).state, UnitState::Empty);
}

#[test]
fn subscription_that_is_a_repo_has_no_loose_unit() {
    let tmp = tempfile::tempdir().unwrap();
    testutil::repo(&tmp.path().join("work/r"));
    let subs = [spec("work/r")];
    let found = side(tmp.path(), &subs);
    let units = snapshot(&found, &found.repos(), false).unwrap();
    assert_eq!(units.len(), 1);
    assert_eq!(units[0].kind, UnitKind::Repo);
    assert_eq!(units[0].rel, "work/r");
}

#[test]
fn single_file_subscription_is_a_file_loose_unit() {
    let tmp = tempfile::tempdir().unwrap();
    testutil::write(&tmp.path().join(".claude/settings.json"), "{}");
    let subs = [SubSpec {
        rel: ".claude/settings.json".into(),
        exclude: vec![],
        except: vec![],
        root: String::new(),
    }];
    let found = side(tmp.path(), &subs);
    let units = snapshot(&found, &[], true).unwrap();
    assert_eq!(units.len(), 1);
    assert!(units[0].is_file);
    assert!(!units[0].state.is_empty());
}

#[test]
fn except_carves_out_repos_and_loose_paths() {
    let tmp = tempfile::tempdir().unwrap();
    testutil::repo(&tmp.path().join("work/rust/sysdirs"));
    testutil::repo(&tmp.path().join("work/tundra/beacon"));
    testutil::write(&tmp.path().join("work/rust/notes.md"), "n");
    let subs = [SubSpec {
        rel: "work".into(),
        exclude: vec![],
        except: vec!["work/rust".into()],
        root: String::new(),
    }];
    let found = side(tmp.path(), &subs);
    assert_eq!(found.repos(), vec!["work/tundra/beacon"]);
    let units = snapshot(&found, &found.repos(), false).unwrap();
    assert!(units.iter().all(|u| !u.rel.starts_with("work/rust")));
    // `work/rust` and `work/tundra` dirs: rust is carved out, tundra remains.
    let with = snapshot(&found, &found.repos(), false).unwrap();
    testutil::write(&tmp.path().join("work/rust/more.md"), "m");
    let found2 = side(tmp.path(), &subs);
    let after = snapshot(&found2, &found2.repos(), false).unwrap();
    assert_eq!(
        find(&with, "work", UnitKind::Loose).state,
        find(&after, "work", UnitKind::Loose).state,
        "changes under an except do not move the loose state"
    );
}

#[test]
fn excludes_do_not_move_state() {
    let tmp = tempfile::tempdir().unwrap();
    testutil::write(&tmp.path().join("work/a.txt"), "a");
    let subs = [spec("work")];
    let before = snapshot(&side(tmp.path(), &subs), &[], false).unwrap();
    testutil::write(&tmp.path().join("work/target/debug/x"), "x");
    let after = snapshot(&side(tmp.path(), &subs), &[], false).unwrap();
    assert_eq!(before, after);
}

#[test]
fn case_check_reports_collisions_relative_to_unit() {
    let tmp = tempfile::tempdir().unwrap();
    testutil::write(&tmp.path().join("work/Readme"), "a");
    testutil::write(&tmp.path().join("work/readme"), "b");
    let subs = [spec("work")];
    let units = snapshot(&side(tmp.path(), &subs), &[], true).unwrap();
    let names = repos::all_names(&tmp.path().join("work"), &Excludes::default()).unwrap();
    if names.len() == 2 {
        assert_eq!(
            units[0].collisions,
            vec![("Readme".into(), "readme".into())]
        );
    } else {
        assert!(
            units[0].collisions.is_empty(),
            "case-insensitive fs holds one name"
        );
    }
    let off = snapshot(&side(tmp.path(), &subs), &[], false).unwrap();
    assert!(off[0].collisions.is_empty(), "not computed unless asked");
}

#[test]
fn join_handles_empty_rel() {
    assert_eq!(join("work", ""), "work");
    assert_eq!(join("work", "a/b"), "work/a/b");
}

/// F4: what "would remove N files" counts on each side.
#[test]
fn units_count_their_files_outside_git() {
    let tmp = tempfile::tempdir().unwrap();
    testutil::tree(&tmp.path().join("work"));
    let found = side(tmp.path(), &[spec("work")]);
    let units = snapshot(&found, &found.repos(), false).unwrap();
    let files = |rel, kind| find(&units, rel, kind).files;
    assert_eq!(files("work/alpha", UnitKind::Repo), 1, "README");
    assert_eq!(
        files("work/beta", UnitKind::Repo),
        2,
        "README tracked, new.txt untracked"
    );
    assert_eq!(
        files("work", UnitKind::Loose),
        2,
        "notes.txt and data/blob.bin; target/ excluded, data/ is a dir"
    );
    // A plain directory where the other side has a repo counts as loose.
    let other = tmp.path().join("other");
    testutil::write(&other.join("work/alpha/x.txt"), "x");
    testutil::write(&other.join("work/alpha/y/z.txt"), "z");
    let plain = side(&other, &[spec("work")]);
    let units = snapshot(&plain, &["work/alpha".into()], false).unwrap();
    assert_eq!(find(&units, "work/alpha", UnitKind::Repo).files, 2);
}
