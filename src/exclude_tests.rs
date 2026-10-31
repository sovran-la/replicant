use super::*;

#[test]
fn bare_name_matches_any_final_component() {
    let e = Excludes::new(&["target", ".DS_Store"]);
    assert!(e.matches("target", true));
    assert!(e.matches("a/b/target", true));
    assert!(e.matches("a/.DS_Store", false));
    assert!(!e.matches("a/targets", true));
    assert!(!e.matches("target/x", false), "matches() ignores ancestors");
    assert!(
        e.path_excluded("target/x", false),
        "path_excluded() does not"
    );
    assert!(e.path_excluded("a/target/deep/x.rs", false));
}

#[test]
fn star_globs_stay_within_a_component() {
    let e = Excludes::new(&["*.jsonl"]);
    assert!(e.matches("a/b.jsonl", false));
    assert!(!e.matches("a/b.json", false));
    let e = Excludes::new(&["a/*/c"]);
    assert!(e.matches("a/b/c", false));
    assert!(!e.matches("a/b/x/c", false));
}

#[test]
fn double_star_crosses_slashes() {
    let e = Excludes::new(&["**/*.sock"]);
    assert!(e.matches("x/y/z.sock", false));
    let e = Excludes::new(&BUILTIN.to_vec());
    assert!(e.matches("repo/.git/index.lock", false));
    assert!(e.matches("repo/.git/refs/heads/main.lock", false));
    assert!(e.matches(".git/HEAD.lock", false));
    assert!(
        !e.matches("repo/Cargo.lock", false),
        "only locks inside .git"
    );
    assert!(e.matches("repo/.git/worktrees", true));
    assert!(!e.matches("repo/.git/worktrees", false), "dir-only");
    assert!(e.path_excluded("repo/.git/worktrees/wt/HEAD", false));
}

#[test]
fn slash_pattern_matches_at_component_boundary_only() {
    let e = Excludes::new(&["agents/state"]);
    assert!(e.matches("agents/state", true));
    assert!(e.matches(".sovran/agents/state", true));
    assert!(!e.matches("xagents/state", true));
}

#[test]
fn anchored_matches_only_from_root() {
    let mut e = Excludes::default();
    e.push_exact("tundra/beacon", true);
    assert!(e.matches("tundra/beacon", true));
    assert!(!e.matches("x/tundra/beacon", true));
    assert!(!e.matches("tundra/beacon", false), "pushed as a directory");
    assert!(e.path_excluded("tundra/beacon/src/main.rs", false));
    assert_eq!(e.rsync_args(), vec!["--exclude=/tundra/beacon/"]);
}

#[test]
fn push_exact_escapes_glob_characters() {
    let mut e = Excludes::default();
    e.push_exact("weird[1]*?", false);
    assert!(e.matches("weird[1]*?", false));
    assert!(!e.matches("weird1xx", false));
}

#[test]
fn classes_and_question_mark() {
    let e = Excludes::new(&["file?.[ch]", "[!a]x"]);
    assert!(e.matches("file1.c", false));
    assert!(e.matches("fileZ.h", false));
    assert!(!e.matches("file1.o", false));
    assert!(e.matches("bx", false));
    assert!(!e.matches("ax", false));
    let e = Excludes::new(&["v[0-9]"]);
    assert!(e.matches("v7", false));
    assert!(!e.matches("vx", false));
}

#[test]
fn unterminated_class_is_literal() {
    let e = Excludes::new(&["a[b"]);
    assert!(e.matches("a[b", false));
}

#[test]
fn for_root_adds_builtins_and_dedupes() {
    let e = Excludes::for_root(&["target".into(), "target".into()]);
    assert_eq!(e.rsync_args().len(), BUILTIN.len() + 1);
}

#[test]
fn trailing_slash_on_path_is_ignored_by_path_excluded() {
    let e = Excludes::new(&["node_modules"]);
    assert!(e.path_excluded("web/node_modules/", true));
}
