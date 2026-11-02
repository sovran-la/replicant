//! Units: what the direction table is decided over.
//!
//! PLAN.md, "Per-path baselines": baselines and the table are per repo and per
//! non-git directory, not tree-wide. v0.1's units are:
//!
//! - one **repo unit** per repo found under a subscription, on *either* side
//!   (the union, so a repo that exists only on home is still a unit here);
//! - one **loose unit** per subscription: every non-repo entry under it, minus
//!   the union's repos. A subscription that is itself a repo has none.
//!
//! Both sides compute their units with [`snapshot`] against the same union, so
//! a unit means the same set of paths on both ends. The leaf discovers first,
//! sends its repo list to home, home unions and answers, and the leaf unions
//! home's list and computes its own side.

use crate::Result;
use crate::exclude::Excludes;
use crate::paths;
use crate::repos::{self, Discovery};
use crate::state::UnitState;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

mod snapshot;

pub(crate) use snapshot::repo_facts;
pub use snapshot::{UnitTiming, snapshot, snapshot_timed};

/// A subscription as both sides see it: its path, its root's excludes, and
/// the host's carve-outs under it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubSpec {
    pub rel: String,
    pub exclude: Vec<String>,
    pub except: Vec<String>,
    /// Its root's path (`.sovran`), which a slash `exclude` is relative to
    /// (review P3). Absent from an older home: empty, and slash patterns
    /// are applied relative to each unit, as they were.
    #[serde(default)]
    pub root: String,
}

impl SubSpec {
    /// `rel`'s path below this subscription's root, `""` at the root.
    pub fn prefix<'a>(&self, rel: &'a str) -> &'a str {
        if self.root.is_empty() {
            return "";
        }
        paths::relative_to(rel, &self.root).unwrap_or("")
    }

    /// The root's excludes as they apply inside the unit at `rel`.
    pub fn excludes_at(&self, rel: &str) -> Result<Excludes> {
        Excludes::for_unit(&self.exclude, self.prefix(rel))
    }

    /// Whether `rel` is inside this subscription and not carved out of it.
    pub fn covers(&self, rel: &str) -> bool {
        paths::is_within(rel, &self.rel) && !excepted(rel, &self.except)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitKind {
    Repo,
    Loose,
}

/// One unit on one side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SideUnit {
    /// Home-relative path; the unit's identity.
    pub rel: String,
    /// The subscription it belongs to.
    pub sub: String,
    pub kind: UnitKind,
    pub state: UnitState,
    /// `.git/HEAD`, when this side has a repo here.
    pub head: Option<String>,
    /// Dirty paths relative to the unit (repos only).
    pub dirty: Vec<String>,
    /// Git lock files relative to the unit (repos only).
    pub locks: Vec<String>,
    /// Case-collision pairs relative to the unit, when asked for.
    pub collisions: Vec<(String, String)>,
    /// Whether this side's path is a single file rather than a directory.
    pub is_file: bool,
    /// Files this side holds in the unit, outside `.git`: tracked plus
    /// untracked for a repo, non-directory entries for loose content. What
    /// "deleted on X — would remove N files on Y" counts.
    #[serde(default)]
    pub files: u64,
    /// What the repo's own `.gitignore` files ignore here, relative to the
    /// unit, minus what the unit's excludes already drop
    /// ([`repos::ignored`]'s form: a directory ends in `/`). Not state, not
    /// walked, and never moved by rsync in either direction: the plan
    /// excludes both sides' lists. Empty for loose units, for a side with
    /// no repo, and from an older home.
    #[serde(default)]
    pub ignored: Vec<String>,
}

/// One side's discovery of every subscription.
#[derive(Debug, Clone)]
pub struct Discovered {
    pub subs: Vec<(SubSpec, PathBuf, Discovery)>,
}

impl Discovered {
    /// Discover each subscription at the path `map` gives it on this side,
    /// following the base if it is a symlink ([`repos::discover_base`]).
    pub fn run(subs: &[SubSpec], map: &dyn Fn(&str) -> PathBuf) -> Result<Self> {
        let mut out = Vec::new();
        for s in subs {
            let abs = map(&s.rel);
            let d = repos::discover_base(&abs, &s.excludes_at(&s.rel)?)?;
            out.push((s.clone(), abs, d));
        }
        Ok(Self { subs: out })
    }

    /// Home-relative repo paths this side found, minus carve-outs.
    pub fn repos(&self) -> Vec<String> {
        self.collect(|d| &d.repos)
    }

    /// Subscriptions whose base does not exist on this side, with the path
    /// looked at. [`repos::discover`] reports absence; the caller decides
    /// what it means (see `plan::bases`).
    pub fn absent(&self) -> Vec<(String, PathBuf)> {
        self.subs
            .iter()
            .filter(|(_, _, d)| !d.exists)
            .map(|(s, abs, _)| (s.rel.clone(), abs.clone()))
            .collect()
    }

    /// Home-relative worktree checkouts this side found.
    pub fn worktrees(&self) -> Vec<String> {
        self.collect(|d| &d.worktrees)
    }

    fn collect(&self, pick: impl Fn(&Discovery) -> &Vec<String>) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (s, _, d) in &self.subs {
            for r in pick(d) {
                let rel = join(&s.rel, r);
                if !excepted(&rel, &s.except) && !out.contains(&rel) {
                    out.push(rel);
                }
            }
        }
        out.sort();
        out
    }
}

/// `sub` joined with a path relative to it.
pub fn join(sub: &str, rel: &str) -> String {
    if rel.is_empty() {
        sub.to_string()
    } else {
        format!("{sub}/{rel}")
    }
}

fn excepted(rel: &str, except: &[String]) -> bool {
    except.iter().any(|e| paths::is_within(rel, e))
}

/// Everything before the first `/`, or the whole thing.
pub(crate) fn first_component(rel: &str) -> &str {
    rel.split('/').next().unwrap_or(rel)
}

/// The exclude list for one unit, relative to the unit's directory: the
/// root's excludes (slash patterns rewritten from the root to the unit) and
/// the built-ins, plus every carve-out and worktree
/// checkout inside it, plus (for a loose unit) every repo inside it, which is
/// a unit of its own. A carve-out matches a file or a directory; repos and
/// worktrees, directories only.
///
/// rsync gets this list too, with *both* sides' worktrees: the source's so a
/// checkout is never sent, the target's so `--delete` never removes one.
pub fn excludes(
    spec: &SubSpec,
    rel: &str,
    kind: UnitKind,
    union_repos: &[String],
    worktrees: &[String],
) -> Result<Excludes> {
    let mut ex = spec.excludes_at(rel)?;
    let repos: &[String] = if kind == UnitKind::Loose {
        union_repos
    } else {
        &[]
    };
    // A carve-out may name a file or a directory, and the loose manifest drops
    // either (`units/snapshot.rs`), so rsync must too: an anchored `/x`
    // matches both. Repos and worktrees are always directories.
    let carve_outs = spec.except.iter().map(|b| (b, false));
    let units = worktrees.iter().chain(repos).map(|b| (b, true));
    for (b, is_dir) in carve_outs.chain(units) {
        if let Some(x) = paths::relative_to(b, rel).filter(|x| !x.is_empty()) {
            ex.push_exact(x, is_dir);
        }
    }
    Ok(ex)
}

/// `ex` plus one anchored literal per git-ignored path
/// ([`SideUnit::ignored`]'s form). Given both sides' lists for a transfer,
/// rsync neither sends an ignored path nor `--delete`s one from the target.
pub fn with_ignored<'a>(
    mut ex: Excludes,
    ignored: impl IntoIterator<Item = &'a String>,
) -> Excludes {
    for p in ignored {
        match p.strip_suffix('/') {
            Some(dir) => ex.push_exact(dir, true),
            None => ex.push_exact(p, false),
        }
    }
    ex
}

#[cfg(test)]
#[path = "units_tests.rs"]
mod tests;
