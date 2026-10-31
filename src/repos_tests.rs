use super::*;
use crate::testutil;

fn work_excludes() -> Excludes {
    Excludes::for_root(&["target".into(), "node_modules".into()])
}

#[test]
fn fixture_tree_discovers_repos_worktree_and_loose() {
    let tmp = tempfile::tempdir().unwrap();
    testutil::tree(tmp.path());
    let d = discover(tmp.path(), &work_excludes()).unwrap();
    assert!(d.exists);
    assert_eq!(d.repos, vec!["alpha", "beta", "nested/gamma"]);
    assert_eq!(d.worktrees, vec!["alpha-wt"]);
    let loose: Vec<_> = d.loose.iter().map(|e| (e.rel.as_str(), e.kind)).collect();
    assert_eq!(
        loose,
        vec![
            ("data", Kind::Dir),
            ("data/blob.bin", Kind::File),
            ("nested", Kind::Dir),
            ("notes.txt", Kind::File),
        ]
    );
    let blob = d.loose.iter().find(|e| e.rel == "data/blob.bin").unwrap();
    assert_eq!(blob.size, 10);
    assert!(blob.mtime > 0);
}

#[test]
fn worktree_is_a_git_file_pointing_into_parent_worktrees() {
    let tmp = tempfile::tempdir().unwrap();
    testutil::tree(tmp.path());
    let wt = tmp.path().join("alpha-wt");
    assert!(wt.join(".git").is_file());
    let text = std::fs::read_to_string(wt.join(".git")).unwrap();
    assert!(text.contains(".git/worktrees/"), "{text}");
    assert_eq!(git_marker(&wt), GitMarker::Worktree);
    assert_eq!(git_marker(&tmp.path().join("alpha")), GitMarker::Repo);
    assert_eq!(git_marker(&tmp.path().join("data")), GitMarker::None);
}

#[test]
fn relative_gitdir_worktree_is_detected() {
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path().join("wt");
    testutil::write(&wt.join(".git"), "gitdir: ../repo/.git/worktrees/wt\n");
    assert_eq!(git_marker(&wt), GitMarker::Worktree);
}

#[test]
fn submodule_git_file_is_not_a_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let sm = tmp.path().join("lib");
    testutil::write(&sm.join(".git"), "gitdir: ../.git/modules/lib\n");
    testutil::write(&sm.join("x.c"), "int x;");
    assert_eq!(git_marker(&sm), GitMarker::OtherFile);
    let d = discover(tmp.path(), &Excludes::default()).unwrap();
    assert!(d.worktrees.is_empty() && d.repos.is_empty());
    assert!(
        d.loose.iter().any(|e| e.rel == "lib/x.c"),
        "walked as content"
    );
}

#[test]
fn base_that_is_a_repo_is_repo_empty_rel() {
    let tmp = tempfile::tempdir().unwrap();
    let r = testutil::repo(&tmp.path().join("r"));
    let d = discover(&r, &Excludes::default()).unwrap();
    assert_eq!(d.repos, vec![""]);
    assert!(d.loose.is_empty());
}

#[test]
fn missing_base_and_single_file_base() {
    let tmp = tempfile::tempdir().unwrap();
    let d = discover(&tmp.path().join("absent"), &Excludes::default()).unwrap();
    assert_eq!(d, Discovery::default());
    let f = tmp.path().join("settings.json");
    testutil::write(&f, "{}");
    let d = discover(&f, &Excludes::default()).unwrap();
    assert!(d.exists);
    assert_eq!(d.loose.len(), 1);
    assert_eq!(d.loose[0].rel, "");
    assert_eq!(d.loose[0].size, 2);
}

#[test]
fn symlinks_are_recorded_not_followed() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("real")).unwrap();
    testutil::write(&tmp.path().join("real/f"), "x");
    std::os::unix::fs::symlink("real", tmp.path().join("link")).unwrap();
    let d = discover(tmp.path(), &Excludes::default()).unwrap();
    let link = d.loose.iter().find(|e| e.rel == "link").unwrap();
    assert_eq!(link.kind, Kind::Symlink);
    assert_eq!(link.link.as_deref(), Some("real"));
    assert!(!d.loose.iter().any(|e| e.rel == "link/f"));
}

#[test]
fn excluded_directories_are_not_descended() {
    let tmp = tempfile::tempdir().unwrap();
    testutil::repo(&tmp.path().join("target/inner-repo"));
    let d = discover(tmp.path(), &work_excludes()).unwrap();
    assert!(
        d.repos.is_empty(),
        "a repo inside an excluded dir is not found"
    );
}

#[test]
fn all_names_descends_into_repos_but_not_objects_or_worktrees() {
    let tmp = tempfile::tempdir().unwrap();
    testutil::tree(tmp.path());
    let names = all_names(tmp.path(), &work_excludes()).unwrap();
    assert!(names.iter().any(|n| n == "alpha/README"));
    assert!(names.iter().any(|n| n == "alpha/.git/HEAD"));
    assert!(!names.iter().any(|n| n.starts_with("alpha/.git/objects/")));
    assert!(!names.iter().any(|n| n.starts_with("alpha-wt/")));
    assert!(!names.iter().any(|n| n.starts_with("target")));
    assert!(
        !names.iter().any(|n| n.ends_with("index.lock")),
        "builtin lock exclude"
    );
}

#[test]
fn case_collisions_finds_pairs_differing_only_by_case() {
    let names = [
        "src/Readme.md",
        "src/README.md",
        "src/main.rs",
        "Docs/a",
        "docs/a",
        "docs/b",
    ];
    assert_eq!(
        case_collisions(names),
        vec![
            ("Docs/a".to_string(), "docs/a".to_string()),
            ("src/README.md".to_string(), "src/Readme.md".to_string()),
        ]
    );
    assert!(
        case_collisions(["a", "b", "a"]).is_empty(),
        "duplicates are not collisions"
    );
}

/// A real collision pair on disk. Only possible on a case-sensitive
/// filesystem; on APFS's default the second create *is* the first file, which
/// is the very merge guardrail 4 exists to refuse. The pure detector above
/// covers the logic on every filesystem.
#[test]
fn case_collision_pair_on_disk_when_filesystem_allows() {
    let tmp = tempfile::tempdir().unwrap();
    testutil::write(&tmp.path().join("Foo.txt"), "upper");
    testutil::write(&tmp.path().join("foo.txt"), "lower");
    let names = all_names(tmp.path(), &Excludes::default()).unwrap();
    let sensitive = names.len() == 2;
    let found = case_collisions(names.iter().map(String::as_str));
    if sensitive {
        assert_eq!(found.len(), 1);
    } else {
        assert!(
            found.is_empty(),
            "insensitive fs merged them: one name on disk"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("Foo.txt")).unwrap(),
            "lower"
        );
    }
}
