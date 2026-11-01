use super::*;

fn ex(patterns: &[&str]) -> Excludes {
    Excludes::new(patterns).unwrap()
}

#[test]
fn bare_name_matches_any_final_component() {
    let e = ex(&["target", ".DS_Store"]);
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
    let e = ex(&["*.jsonl"]);
    assert!(e.matches("a/b.jsonl", false));
    assert!(!e.matches("a/b.json", false));
    let e = ex(&["a/*/c"]);
    assert!(e.matches("a/b/c", false));
    assert!(!e.matches("a/b/x/c", false));
}

#[test]
fn double_star_crosses_slashes_and_matches_zero_directories() {
    let e = ex(&["**/*.sock"]);
    assert!(e.matches("x/y/z.sock", false));
    assert!(e.matches("z.sock", false), "`**/` is zero or more");
    let e = ex(&["a/**/b"]);
    assert!(e.matches("a/b", false));
    assert!(e.matches("a/x/y/b", false));
    assert!(!e.matches("x/a/b", false));
    let e = ex(&["cache/**"]);
    assert!(e.matches("cache/x/y", false));
    assert!(!e.matches("cache", true), "the directory itself stays");
}

#[test]
fn builtins_cover_git_locks_and_worktrees_at_any_depth() {
    let e = ex(BUILTIN);
    assert!(e.matches("repo/.git/index.lock", false));
    assert!(e.matches("repo/.git/refs/heads/main.lock", false));
    assert!(e.matches(".git/HEAD.lock", false));
    assert!(e.matches("a/b/.git/x/y/z.lock", false));
    assert!(
        !e.matches("repo/Cargo.lock", false),
        "only locks inside .git"
    );
    assert!(!e.matches(".git/index", false));
    assert!(e.matches("repo/.git/worktrees", true));
    assert!(e.matches(".git/worktrees", true));
    assert!(!e.matches("repo/.git/worktrees", false), "dir-only");
    assert!(e.path_excluded("repo/.git/worktrees/wt/HEAD", false));
}

/// The filesystem crate's rule: a pattern with a `/` is matched against the
/// unit-relative path, so it is rooted at the unit. (rsync's own unanchored
/// rule matched it at any depth; `**/` says that now.)
#[test]
fn slash_pattern_is_rooted_at_the_unit() {
    let e = ex(&["agents/state"]);
    assert!(e.matches("agents/state", true));
    assert!(!e.matches(".sovran/agents/state", true));
    assert!(!e.matches("xagents/state", true));
    let e = ex(&["**/agents/state"]);
    assert!(e.matches(".sovran/agents/state", true));
    assert!(e.matches("agents/state", true));
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
    let e = ex(&["/top"]);
    assert!(e.matches("top", false));
    assert!(!e.matches("app/top", false), "never the bare name");
}

#[test]
fn push_exact_escapes_glob_characters() {
    let mut e = Excludes::default();
    e.push_exact("weird[1]*?{a,b}", false);
    assert!(e.matches("weird[1]*?{a,b}", false));
    assert!(!e.matches("weird1xx", false));
    assert!(!e.matches("weirda", false));
}

/// Every printable ASCII character, alone and in company, renders to a glob
/// that compiles and matches only itself; `compile` relies on this.
#[test]
fn push_exact_renders_every_ascii_character() {
    for b in 0x20u8..0x7f {
        let c = b as char;
        if c == '/' {
            continue;
        }
        for name in [c.to_string(), format!("a{c}b"), format!("{c}{c}")] {
            let mut e = Excludes::default();
            e.push_exact(&name, false);
            assert!(e.matches(&name, false), "{name:?}");
            assert!(!e.matches("plain", false), "{name:?}");
            assert_eq!(e.rsync_args().len(), 1, "{name:?}");
        }
    }
}

#[test]
fn positive_ascii_classes() {
    let e = ex(&["file[0-9].[ch]"]);
    assert!(e.matches("file1.c", false));
    assert!(e.matches("file7.h", false));
    assert!(!e.matches("file1.o", false));
    assert!(!e.matches("fileZ.c", false));
    let e = ex(&["v[0-9]"]);
    assert!(e.matches("v7", false));
    assert!(!e.matches("vx", false));
    let e = ex(&["[*?]x"]);
    assert!(e.matches("*x", false), "specials are literal in a class");
}

#[test]
fn dir_only_ignores_files_and_links() {
    let e = ex(&["build/"]);
    assert!(e.matches("build", true));
    assert!(e.matches("a/build", true));
    assert!(!e.matches("a/build", false));
}

#[test]
fn refused_patterns_say_why() {
    let cases = [
        ("", "empty"),
        (" target", "whitespace"),
        ("target ", "whitespace"),
        ("a\nb", "line breaks"),
        ("!", "filter-rule prefix"),
        ("+ keep", "filter-rule prefix"),
        ("- drop", "filter-rule prefix"),
        ("/", "transfer root"),
        ("file?", "one byte"),
        ("[!a]x", "negated"),
        ("[^a]x", "negated"),
        ("{a,b}", "alternation"),
        ("a}", "alternation"),
        ("a**", "whole path component"),
        ("**a", "whole path component"),
        ("a/**b/c", "whole path component"),
        (".git/**.lock", "whole path component"),
        ("***", "rsync-only"),
        ("a//b", "empty path component"),
        ("a[b", "unterminated"),
        ("a]", "unmatched"),
        ("[]a]", "empty"),
        ("[a-]", "either end"),
        ("[-a]", "either end"),
        ("[z-a]", "invalid range"),
        ("[a-b-c]", "invalid range"),
        ("[[:alpha:]]", "class"),
        ("[a/b]", "class"),
        ("[é]", "class"),
        ("a\\", "escapes nothing"),
        ("a\\/b", "not supported"),
        ("**/**/**/**/**/x", "more than 4"),
    ];
    for (pat, why) in cases {
        let err = check(pat).expect_err(pat);
        assert!(err.contains(why), "{pat:?}: {err}");
        let err = Excludes::new(&[pat]).unwrap_err().to_string();
        assert!(
            err.contains(why) && err.contains("exclude pattern"),
            "{pat:?}: {err}"
        );
    }
}

#[test]
fn plan_examples_are_accepted() {
    for p in [
        "target",
        ".build",
        "node_modules",
        "bin",
        ".DS_Store",
        "projects",
        "todos",
        "statsig",
        "*.jsonl",
        ".credentials.json",
        "agents/state",
        "**/*.sock",
        "**/*.lock",
        "**/*.log",
    ] {
        check(p).unwrap_or_else(|e| panic!("{p}: {e}"));
    }
}

/// The translation table in `docs/v0.2-prep-notes.md`, held to the code.
#[test]
fn rsync_rules_follow_the_translation_table() {
    let rules = |p: &str| {
        let e = ex(&[p]);
        e.rsync_args()
            .into_iter()
            .map(|a| a.trim_start_matches("--exclude=").to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(rules("target"), ["target"]);
    assert_eq!(rules("*.log"), ["*.log"]);
    assert_eq!(rules("build/"), ["build/"]);
    assert_eq!(rules("/top"), ["/top"]);
    assert_eq!(rules("agents/state"), ["/agents/state"]);
    assert_eq!(rules("**/x"), ["/x", "/**/x"]);
    assert_eq!(rules("a/**/b/"), ["/a/b/", "/a/**/b/"]);
    assert_eq!(rules("a/**"), ["/a/**"]);
    assert_eq!(rules("**"), ["/**"]);
    assert_eq!(
        rules("**/.git/**/*.lock"),
        [
            "/.git/*.lock",
            "/**/.git/*.lock",
            "/.git/**/*.lock",
            "/**/.git/**/*.lock"
        ]
    );
    assert_eq!(rules("v[0-9]"), ["v[0-9]"]);
    assert_eq!(rules("\\*lit"), ["\\*lit"]);
    // No wildcard anywhere: rsync reads a backslash literally.
    assert_eq!(rules("we\\\\ird"), ["we\\ird"]);
    // A wildcard elsewhere turns escapes on, so the backslash is doubled.
    assert_eq!(rules("we\\\\ird*"), ["we\\\\ird*"]);
    assert_eq!(rules("\\{x\\}"), ["{x}"]);
}

#[test]
fn for_root_adds_builtins_and_dedupes() {
    let twice = Excludes::for_root(&["target".into(), "target".into()]).unwrap();
    let once = Excludes::for_root(&["target".into()]).unwrap();
    assert_eq!(twice, once);
    assert_eq!(twice.rsync_args(), once.rsync_args());
    let builtins = Excludes::new(BUILTIN).unwrap().rsync_args().len();
    assert_eq!(once.rsync_args().len(), builtins + 1);
}

#[test]
fn equality_ignores_compilation_state() {
    let a = ex(&["target"]);
    let b = ex(&["target"]);
    assert!(a.matches("target", true), "compiles a");
    assert_eq!(a, b);
    assert_ne!(a, ex(&["target/"]));
}

#[test]
fn push_after_matching_recompiles() {
    let mut e = ex(&["target"]);
    assert!(!e.matches("node_modules", true));
    e.push("node_modules").unwrap();
    assert!(e.matches("node_modules", true));
}

#[test]
fn trailing_slash_on_path_is_ignored_by_path_excluded() {
    let e = ex(&["node_modules"]);
    assert!(e.path_excluded("web/node_modules/", true));
}

#[test]
fn empty_excludes_match_nothing() {
    let e = Excludes::default();
    assert!(e.is_empty());
    assert!(!e.path_excluded("a/b", false));
    assert!(e.rsync_args().is_empty());
    let mut e = Excludes::default();
    e.push_exact("", true);
    assert!(e.is_empty(), "an empty exact path is no pattern");
}
