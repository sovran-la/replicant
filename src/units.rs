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
use crate::state::{self, UnitState};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A subscription as both sides see it: its path, its root's excludes, and
/// the host's carve-outs under it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubSpec {
    pub rel: String,
    pub exclude: Vec<String>,
    pub except: Vec<String>,
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
}

/// One side's discovery of every subscription.
#[derive(Debug, Clone)]
pub struct Discovered {
    pub subs: Vec<(SubSpec, PathBuf, Discovery)>,
}

impl Discovered {
    /// Discover each subscription at the path `map` gives it on this side.
    pub fn run(subs: &[SubSpec], map: &dyn Fn(&str) -> PathBuf) -> Result<Self> {
        let mut out = Vec::new();
        for s in subs {
            let abs = map(&s.rel);
            let d = repos::discover(&abs, &Excludes::for_root(&s.exclude))?;
            out.push((s.clone(), abs, d));
        }
        Ok(Self { subs: out })
    }

    /// Home-relative repo paths this side found, minus carve-outs.
    pub fn repos(&self) -> Vec<String> {
        self.collect(|d| &d.repos)
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
fn first_component(rel: &str) -> &str {
    rel.split('/').next().unwrap_or(rel)
}

/// The exclude list for one unit, relative to the unit's directory: the
/// root's excludes and the built-ins, plus every carve-out and worktree
/// checkout inside it, plus (for a loose unit) every repo inside it, which is
/// a unit of its own.
///
/// rsync gets this list too, with *both* sides' worktrees: the source's so a
/// checkout is never sent, the target's so `--delete` never removes one.
pub fn excludes(
    spec: &SubSpec,
    rel: &str,
    kind: UnitKind,
    union_repos: &[String],
    worktrees: &[String],
) -> Excludes {
    let mut ex = Excludes::for_root(&spec.exclude);
    let repos: &[String] = if kind == UnitKind::Loose {
        union_repos
    } else {
        &[]
    };
    for b in spec.except.iter().chain(worktrees).chain(repos) {
        if let Some(x) = paths::relative_to(b, rel).filter(|x| !x.is_empty()) {
            ex.push_exact(x, true);
        }
    }
    ex
}

/// This side's units, against the union of both sides' repos.
pub fn snapshot(
    found: &Discovered,
    union_repos: &[String],
    case_check: bool,
) -> Result<Vec<SideUnit>> {
    snapshot_timed(found, union_repos, case_check).map(|(u, _)| u)
}

/// How many of the slowest units `snapshot_timed` names.
const SLOWEST: usize = 5;

/// A unit's home-relative path and how long its hash took, in ms.
pub type UnitTiming = (String, u128);

/// [`snapshot`], plus the slowest units and their wall time in ms.
pub fn snapshot_timed(
    found: &Discovered,
    union_repos: &[String],
    case_check: bool,
) -> Result<(Vec<SideUnit>, Vec<UnitTiming>)> {
    struct Job<'a> {
        spec: &'a SubSpec,
        abs: PathBuf,
        rel: String,
        kind: UnitKind,
        loose: Vec<repos::Entry>,
        is_file: bool,
        excludes: Excludes,
    }
    let mut jobs = Vec::new();
    for (spec, sub_abs, d) in &found.subs {
        let repos_here: Vec<&String> = union_repos
            .iter()
            .filter(|r| paths::is_within(r, &spec.rel))
            .collect();
        for r in &repos_here {
            let inner = paths::relative_to(r, &spec.rel).unwrap_or("");
            let ex = excludes(spec, r, UnitKind::Repo, union_repos, &[]);
            jobs.push(Job {
                spec,
                abs: if inner.is_empty() {
                    sub_abs.clone()
                } else {
                    sub_abs.join(inner)
                },
                rel: (*r).clone(),
                kind: UnitKind::Repo,
                loose: Vec::new(),
                is_file: false,
                excludes: ex,
            });
        }
        if repos_here.iter().any(|r| **r == spec.rel) {
            continue;
        }
        // Blocked paths, made relative to the subscription so each loose entry
        // is tested as-is, and bucketed by first component so an entry is
        // compared only with the few that could contain it. The plain scan
        // was 70k entries × 154 repos on thistledown: two seconds, serial.
        let mut blocked: HashMap<&str, Vec<&str>> = HashMap::new();
        for b in repos_here
            .iter()
            .map(|r| r.as_str())
            .chain(spec.except.iter().map(String::as_str))
        {
            if let Some(inner) = paths::relative_to(b, &spec.rel).filter(|x| !x.is_empty()) {
                blocked
                    .entry(first_component(inner))
                    .or_default()
                    .push(inner);
            }
        }
        let loose = d
            .loose
            .iter()
            .filter(|e| {
                blocked
                    .get(first_component(&e.rel))
                    .is_none_or(|bs| !bs.iter().any(|b| paths::is_within(&e.rel, b)))
            })
            .cloned()
            .collect();
        let is_file = d.loose.len() == 1 && d.loose[0].rel.is_empty();
        let ex = excludes(spec, &spec.rel, UnitKind::Loose, union_repos, &[]);
        jobs.push(Job {
            spec,
            abs: sub_abs.clone(),
            rel: spec.rel.clone(),
            kind: UnitKind::Loose,
            loose,
            is_file,
            excludes: ex,
        });
    }

    let results = state::parallel_map(&jobs, state::workers(), |j| -> Result<(SideUnit, u128)> {
        let t = std::time::Instant::now();
        let mut u = SideUnit {
            rel: j.rel.clone(),
            sub: j.spec.rel.clone(),
            kind: j.kind,
            state: UnitState::Empty,
            head: None,
            dirty: Vec::new(),
            locks: Vec::new(),
            collisions: Vec::new(),
            is_file: j.is_file,
        };
        match j.kind {
            UnitKind::Repo => repo_side(&j.abs, &j.excludes, &mut u)?,
            UnitKind::Loose => u.state = state::loose(&j.loose),
        }
        let hash_ms = t.elapsed().as_millis();
        if case_check && !j.is_file {
            let names = repos::all_names(&j.abs, &j.excludes)?;
            u.collisions = repos::case_collisions(names.iter().map(String::as_str));
        }
        let total = t.elapsed().as_millis();
        if total >= 100 {
            eprintln!(
                "  unit {}: hash {hash_ms}ms, case walk {}ms",
                j.rel,
                total - hash_ms
            );
        }
        Ok((u, total))
    });
    let timed: Vec<(SideUnit, u128)> = results.into_iter().collect::<Result<_>>()?;
    let mut slowest: Vec<(String, u128)> =
        timed.iter().map(|(u, ms)| (u.rel.clone(), *ms)).collect();
    slowest.sort_by(|a, b| b.1.cmp(&a.1));
    slowest.truncate(SLOWEST);
    Ok((timed.into_iter().map(|(u, _)| u).collect(), slowest))
}

/// Fill a repo unit: a real repo here, a plain directory where the other side
/// has a repo, or nothing.
fn repo_side(abs: &Path, ex: &Excludes, u: &mut SideUnit) -> Result<()> {
    match repos::git_marker(abs) {
        repos::GitMarker::Repo => {
            let f = state::repo(abs, ex)?;
            u.state = f.state;
            u.head = Some(f.head);
            u.dirty = f.dirty;
            u.locks = f.locks;
        }
        _ => {
            let d = repos::discover(abs, ex)?;
            // Not a repo on this side: hash what is there as loose content, so
            // it can never equal a repo's hash.
            u.state = match state::loose(&d.loose) {
                UnitState::Present(h) => UnitState::Present(format!("notrepo-{h}")),
                UnitState::Empty => UnitState::Empty,
            };
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "units_tests.rs"]
mod tests;
