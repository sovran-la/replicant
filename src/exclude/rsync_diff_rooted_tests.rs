//! Review P3: root-relative slash patterns, rewritten per unit, against the
//! system rsync. The fixture is a *root*; each unit is a directory inside it,
//! and rsync transfers only that directory, as a real unit's dry run does.
//! What [`Excludes::for_unit`] drops inside the unit must equal what rsync
//! leaves out given its `rsync_args()`, and both must equal the unrewritten
//! pattern matched root-relatively over the whole root, and a hand-written set.

use super::*;

/// Every fixture path strictly inside `unit`, relative to it.
fn inside(unit: &str) -> BTreeSet<(String, bool)> {
    all()
        .into_iter()
        .filter_map(|(p, d)| {
            crate::paths::relative_to(&p, unit)
                .filter(|r| !r.is_empty())
                .map(|r| (r.to_string(), d))
        })
        .collect()
}

/// rsync over `src/<unit>/` alone: what it leaves out of the unit.
fn theirs_in(src: &Path, unit: &str, ex: &Excludes) -> BTreeSet<String> {
    let from = src.join(unit);
    let dst = src.parent().unwrap().join("dst");
    let out = Command::new("rsync")
        .args(["-n", "-a", "-8", "--itemize-changes", "--out-format=%n"])
        .args(ex.rsync_args())
        .arg(format!("{}/", from.display()))
        .arg(format!("{}/", dst.display()))
        .output()
        .expect("the differential test needs rsync on PATH");
    assert!(out.status.success(), "rsync failed: {out:?}");
    let sent: BTreeSet<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| l.trim_end_matches('/').to_string())
        .filter(|l| !l.is_empty() && l != ".")
        .collect();
    inside(unit)
        .into_iter()
        .map(|(p, _)| p)
        .filter(|p| !sent.contains(p))
        .collect()
}

/// `pat` applied in `unit`: ours, rsync's and the root-relative reference
/// must agree; returns the set.
fn agree_in(pat: &str, unit: &str) -> BTreeSet<String> {
    let label = format!("{pat:?} in unit {unit:?}");
    let ex = Excludes::for_unit(&[pat.to_string()], unit).unwrap();
    let ours: BTreeSet<String> = inside(unit)
        .into_iter()
        .filter(|(p, d)| ex.path_excluded(p, *d))
        .map(|(p, _)| p)
        .collect();
    let rooted = Excludes::new(&[pat]).unwrap();
    let reference: BTreeSet<String> = inside(unit)
        .into_iter()
        .filter(|(p, d)| rooted.path_excluded(&format!("{unit}/{p}"), *d))
        .map(|(p, _)| p)
        .collect();
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir(&src).unwrap();
    build(&src);
    let rsync = theirs_in(&src, unit, &ex);
    assert_eq!(
        ours,
        rsync,
        "{label}: replicant (left) and {} (right) disagree\nrules: {:?}",
        flavour(),
        ex.rsync_args()
    );
    assert_eq!(
        ours, reference,
        "{label}: not what the root-relative pattern means"
    );
    ours
}

#[test]
fn a_root_relative_pattern_under_three_unit_roots() {
    // Stripped, dropped, and covering the unit whole.
    assert_eq!(
        agree_in("agents/state", "agents"),
        set(&["state", "state/db"])
    );
    assert_eq!(agree_in("agents/state", ".sovran"), set(&[]));
    assert_eq!(agree_in("agents/state", "agents/state"), set(&["db"]));
    // Its `.sovran` twin only matches root-relatively from `.sovran`'s side.
    assert_eq!(
        agree_in(".sovran/agents/state", ".sovran/agents"),
        set(&["state", "state/db"])
    );
}

#[test]
fn wildcards_and_globstars_rewrite_under_several_unit_roots() {
    assert_eq!(
        agree_in("a/**/b", "a"),
        set(&["b", "x/b", "x/b/f", "x/y/b"])
    );
    assert_eq!(agree_in("a/**/b", "a/x"), set(&["b", "b/f", "y/b"]));
    assert_eq!(agree_in("a/**/b", "q"), set(&[]));
    assert_eq!(agree_in("q/*/m", "q"), set(&["x/m", "x/m/n", "x/m/n/y"]));
    assert_eq!(agree_in("q/*/m", "q/x"), set(&["m", "m/n", "m/n/y"]));
    assert_eq!(agree_in("q/*/m", "q/x/m"), set(&["n", "n/y"]));
    assert_eq!(
        agree_in("cache/**", "cache"),
        set(&["one", "sub", "sub/two"])
    );
    assert_eq!(agree_in("cache/**", "app"), set(&[]));
    assert_eq!(agree_in("src/*.rs", "lib"), set(&[]));
    assert_eq!(agree_in("lib/src/", "lib"), set(&["src", "src/x.rs"]));
}

#[test]
fn names_and_globstar_patterns_still_match_at_any_depth_in_a_unit() {
    assert_eq!(
        agree_in("target", "app"),
        set(&["target", "target/x", "deep/er/target"])
    );
    assert_eq!(
        agree_in("**/agents/state", ".sovran"),
        set(&["agents/state", "agents/state/db"])
    );
    // A unit whose own name is excluded is covered whole.
    assert_eq!(agree_in("target", "target"), set(&["debug.o"]));
}

#[test]
fn covering_names_the_pattern_that_leaves_nothing_to_sync() {
    let list: Vec<String> = ["target", "agents/state", "cache/**", "**/gen"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let at = |prefix: &str| crate::exclude::covering(&list, prefix).unwrap();
    assert_eq!(at(""), None, "the root itself is never covered");
    assert_eq!(at("agents"), None);
    assert_eq!(at("agents/state").as_deref(), Some("agents/state"));
    assert_eq!(at("agents/state/x/y").as_deref(), Some("agents/state"));
    assert_eq!(at("app/target/x").as_deref(), Some("target"));
    assert_eq!(at("cache").as_deref(), Some("cache/**"));
    assert_eq!(at("p/gen").as_deref(), Some("**/gen"));
    assert_eq!(
        at(".sovran/agents/state"),
        None,
        "root-relative, not any depth"
    );
}
