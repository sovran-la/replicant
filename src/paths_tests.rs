use super::*;
use std::path::PathBuf;

fn roots() -> Vec<String> {
    vec!["work".into(), ".claude".into(), ".sovran".into()]
}

fn mac_leaf_linux_home(roots: &[String]) -> Normalizer<'_> {
    Normalizer {
        leaf_home: Path::new("/Users/morgan"),
        home_home: Path::new("/home/morgan"),
        roots,
    }
}

#[test]
fn rule1_tilde_is_stripped() {
    let r = roots();
    let n = mac_leaf_linux_home(&r);
    assert_eq!(n.one("~/work/tundra").unwrap(), "work/tundra");
    assert!(
        n.one("~").is_err(),
        "~ alone is the home dir, not under a root"
    );
    assert!(n.one("~bob/work").is_err());
}

#[test]
fn rule2_leaf_home_dir_is_stripped() {
    let r = roots();
    let n = mac_leaf_linux_home(&r);
    assert_eq!(n.one("/Users/morgan/work/sequoia").unwrap(), "work/sequoia");
    assert_eq!(
        n.one("/Users/morgan/.claude/settings.json").unwrap(),
        ".claude/settings.json"
    );
}

#[test]
fn rule3_home_home_dir_is_stripped_too() {
    let r = roots();
    let n = mac_leaf_linux_home(&r);
    // Typed on thistledown from muscle memory, on behalf of a mac leaf.
    assert_eq!(n.one("/home/morgan/work/tundra").unwrap(), "work/tundra");
}

#[test]
fn absolute_outside_both_home_dirs_is_refused() {
    let r = roots();
    let n = mac_leaf_linux_home(&r);
    let err = n.one("/opt/work/x").unwrap_err();
    assert!(
        err.contains("/Users/morgan") && err.contains("/home/morgan"),
        "{err}"
    );
    // A sibling that shares a string prefix is not under the home dir.
    assert!(n.one("/Users/morganle/work").is_err());
}

#[test]
fn rule4_trailing_slashes_and_dots_dropped() {
    let r = roots();
    let n = mac_leaf_linux_home(&r);
    assert_eq!(n.one("~/work/tundra/").unwrap(), "work/tundra");
    assert_eq!(n.one("~/work//tundra///").unwrap(), "work/tundra");
    assert_eq!(n.one("work/./tundra").unwrap(), "work/tundra");
    assert_eq!(n.one("  ~/work/tundra  ").unwrap(), "work/tundra");
}

#[test]
fn rule4_dotdot_refused_even_if_it_would_resolve_inside() {
    let r = roots();
    let n = mac_leaf_linux_home(&r);
    assert!(
        n.one("~/work/tundra/../sequoia")
            .unwrap_err()
            .contains("..")
    );
    assert!(n.one("/Users/morgan/../morgan/work").is_err());
}

#[test]
fn rule4_duplicates_collapse_in_first_seen_order() {
    let r = roots();
    let n = mac_leaf_linux_home(&r);
    let got = n
        .normalize(&[
            "/Users/morgan/work/sequoia".into(),
            "~/work/tundra".into(),
            "/home/morgan/work/sequoia/".into(),
        ])
        .unwrap();
    assert_eq!(got, vec!["work/sequoia", "work/tundra"]);
}

#[test]
fn rule5_outside_roots_refused_with_roots_listed() {
    let r = roots();
    let n = mac_leaf_linux_home(&r);
    let err = n.one("~/Downloads/x").unwrap_err();
    assert!(err.contains("work, .claude, .sovran"), "{err}");
    // `work2` shares a prefix with root `work` but is not under it.
    assert!(n.one("~/work2/tundra").is_err());
    let none: Vec<String> = vec![];
    let n = mac_leaf_linux_home(&none);
    assert!(n.one("~/work").unwrap_err().contains("none configured"));
}

#[test]
fn root_itself_is_a_valid_path() {
    let r = roots();
    let n = mac_leaf_linux_home(&r);
    assert_eq!(n.one("~/work").unwrap(), "work");
}

#[test]
fn normalize_reports_every_bad_path() {
    let r = roots();
    let n = mac_leaf_linux_home(&r);
    let err = n
        .normalize(&["~/work/ok".into(), "~/nope".into(), "/etc/passwd".into()])
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("~/nope") && err.contains("/etc/passwd"),
        "{err}"
    );
    assert!(!err.contains("~/work/ok"), "{err}");
}

#[test]
fn same_home_dir_on_both_ends() {
    let r = roots();
    let mac = PathBuf::from("/Users/morgan");
    let n = Normalizer {
        leaf_home: &mac,
        home_home: &mac,
        roots: &r,
    };
    assert_eq!(n.one("/Users/morgan/work/x").unwrap(), "work/x");
}

#[test]
fn nested_home_dirs_prefer_the_longer() {
    let r = vec!["work".to_string()];
    let leaf = PathBuf::from("/home");
    let home = PathBuf::from("/home/morgan");
    let n = Normalizer {
        leaf_home: &leaf,
        home_home: &home,
        roots: &r,
    };
    assert_eq!(n.one("/home/morgan/work/a").unwrap(), "work/a");
}

#[test]
fn is_within_is_component_wise() {
    assert!(is_within("work", "work"));
    assert!(is_within("work/a", "work"));
    assert!(is_within("work/a/b", "work/a"));
    assert!(!is_within("work2", "work"));
    assert!(!is_within("wor", "work"));
    assert!(!is_within("work", "work/a"));
}

#[test]
fn relative_to_strips_parent() {
    assert_eq!(relative_to("work/a/b", "work"), Some("a/b"));
    assert_eq!(relative_to("work", "work"), Some(""));
    assert_eq!(relative_to("work2", "work"), None);
}

#[test]
fn check_rel_rejects_non_normalized() {
    assert!(check_rel("work/a").is_ok());
    for bad in ["", "/work", "work/", "work//a", "./work", "work/../x"] {
        assert!(check_rel(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn collapse_keeps_broadest_and_reports_absorbed() {
    let list: Vec<String> = [
        "work/tundra",
        "work",
        "work/sequoia",
        ".claude/skills",
        "work",
    ]
    .map(String::from)
    .into();
    let (kept, absorbed) = collapse(&list);
    assert_eq!(kept, vec!["work", ".claude/skills"]);
    assert_eq!(absorbed, vec!["work/tundra", "work/sequoia"]);
}

#[test]
fn collapse_with_no_nesting_is_identity() {
    let list: Vec<String> = ["a/x", "a/y"].map(String::from).into();
    assert_eq!(collapse(&list), (list.clone(), vec![]));
}
