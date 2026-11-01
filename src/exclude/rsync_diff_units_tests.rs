//! The unit exclude list ([`crate::units::excludes`]) against the system
//! rsync and against the loose manifest, which drops every path within a
//! carve-out (`units/snapshot.rs`). Review P1: all three must agree, for a
//! carved-out file as much as a directory.

use super::*;
use crate::units::{SubSpec, UnitKind, excludes};

/// The fixture is the unit `u`; carve-outs are root-relative, as in fleet.toml.
const EXCEPT: &[&str] = &["u/top", "u/a/x", "u/{brace}", "u/q/x/m/n/y"];

fn unit_list() -> Excludes {
    let spec = SubSpec {
        rel: "u".into(),
        exclude: Vec::new(),
        except: EXCEPT.iter().map(|s| s.to_string()).collect(),
    };
    excludes(&spec, "u", UnitKind::Loose, &[], &[]).unwrap()
}

/// What `for_root` drops before any carve-out: the built-ins.
fn builtins() -> BTreeSet<String> {
    ours(&Excludes::for_root(&[]).unwrap())
}

/// What `ex` drops beyond the built-ins, after checking rsync agrees.
fn carved(ex: &Excludes, label: &str) -> BTreeSet<String> {
    let got = agree(ex, label);
    got.difference(&builtins()).cloned().collect()
}

/// What the loose manifest drops: every path within a carve-out.
fn manifest_drops() -> BTreeSet<String> {
    all()
        .into_iter()
        .map(|(p, _)| p)
        .filter(|p| {
            EXCEPT
                .iter()
                .filter_map(|x| crate::paths::relative_to(x, "u"))
                .any(|x| crate::paths::is_within(p, x))
        })
        .collect()
}

#[test]
fn carved_out_files_and_directories_agree_with_rsync_and_the_manifest() {
    let got = carved(&unit_list(), "except");
    assert_eq!(
        got,
        set(&[
            "top",
            "a/x",
            "a/x/b",
            "a/x/b/f",
            "a/x/y",
            "a/x/y/b",
            "{brace}",
            "q/x/m/n/y",
        ])
    );
    assert_eq!(got, manifest_drops(), "the hash and the transfer agree");
}

/// A carved-out file leaves its namesakes deeper down alone: `/top` is
/// anchored, so `app/top` still syncs.
#[test]
fn a_carved_out_file_is_anchored() {
    let ex = unit_list();
    assert!(ex.path_excluded("top", false));
    assert!(!ex.path_excluded("app/top", false));
}

/// Repos and worktrees stay directory-only: a *file* at a repo's path (it
/// was replaced, or is mid-checkout) is not a unit and is not hidden.
#[test]
fn repos_and_worktrees_stay_directory_only() {
    let spec = SubSpec {
        rel: "u".into(),
        exclude: Vec::new(),
        except: Vec::new(),
    };
    let ex = excludes(&spec, "u", UnitKind::Loose, &["u/top".into()], &[]).unwrap();
    assert!(carved(&ex, "repo").is_empty(), "`top` is a file");
    let ex = excludes(&spec, "u", UnitKind::Loose, &[], &["u/top".into()]).unwrap();
    assert!(carved(&ex, "worktree").is_empty(), "`top` is a file");
    let ex = excludes(&spec, "u", UnitKind::Loose, &["u/a/x".into()], &[]).unwrap();
    assert!(carved(&ex, "repo dir").contains("a/x/b/f"));
}
