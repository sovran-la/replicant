//! Exclude patterns: globset matching.
//!
//! One list serves every walker of a unit (discovery, the state hash's
//! porcelain filtering, the case walk, and the transfer's manifests on both
//! sides), so they all agree on what is excluded, exactly. Guardrail 1
//! compares the dry run's changes with the dirty lists on that basis.
//!
//! Patterns are globset globs with `*` kept inside one component
//! (`literal_separator`), matched the way the sibling `filesystem` crate's
//! `WalkOptions::is_excluded` matches them: against the path relative to the
//! unit root, or against the bare file name. So:
//!
//! - `target`, `*.jsonl`: a name, at any depth
//! - `agents/state`, `src/*.rs`: a path from the *root* (it has a `/`);
//!   [`Excludes::for_unit`] rewrites it for each unit (review P3)
//! - `**/x/y`, `a/**/b`: `**` is any number of directories, including none
//! - `/x`: anchored, the unit-relative path only (only generated internally;
//!   fleet.toml refuses anchored patterns, see [`crate::config::fleet`])
//! - a trailing `/`: directories only
//! - `[a-z0-9]`: a positive ASCII class
//!
//! The filesystem crate also tries the absolute path. Replicant does not: one
//! unit lives at `/Users/...` on a Mac and `/home/...` on Linux, so an
//! absolute match would make the two sides disagree.
//!
//! `pattern.rs` refuses at config load what is outside the accepted subset:
//! `?` and negated classes, `{a,b}`, `**` inside a component, `***`,
//! unterminated classes and filter-rule prefixes.
//!
//! No walker descends into an excluded directory, so a path is excluded if it
//! or any ancestor matches; [`Excludes::path_excluded`] checks that.

mod pattern;

use crate::{Error, Result};
use globset::{Candidate, GlobBuilder, GlobSet, GlobSetBuilder};
use std::sync::OnceLock;

/// What is never synced, whatever the root says. PLAN.md, "Exclusions".
///
/// `target/`, `node_modules/` and friends are per-root (fleet.toml's `exclude`)
/// rather than built in, because `bin/` and `.build/` are real source in some
/// trees and the plan's example config lists them per root.
///
/// `.replicant-tmp-*` is a file the native receiver is still writing (or a
/// dead transfer left): never state, never sent, removed by the next
/// transfer of its unit (`native::TEMP_PREFIX`).
pub const BUILTIN: &[&str] = &[
    "**/.git/**/*.lock",
    "**/.git/worktrees/",
    ".replicant-tmp-*",
];

#[derive(Debug, Clone)]
struct Pattern {
    /// The canonical form, for de-duplication and equality.
    key: String,
    glob: globset::Glob,
    anchored: bool,
    dir_only: bool,
}

/// The four sets one entry is tested against, built on first use.
#[derive(Debug, Clone)]
struct Compiled {
    /// Unanchored: the relative path or the bare name.
    free: GlobSet,
    free_dir: GlobSet,
    /// Anchored: the relative path only.
    rooted: GlobSet,
    rooted_dir: GlobSet,
}

/// A list of patterns, applied relative to one unit (transfer) root.
#[derive(Debug, Clone, Default)]
pub struct Excludes {
    patterns: Vec<Pattern>,
    compiled: OnceLock<Compiled>,
}

impl PartialEq for Excludes {
    fn eq(&self, other: &Self) -> bool {
        self.patterns.len() == other.patterns.len()
            && self
                .patterns
                .iter()
                .zip(&other.patterns)
                .all(|(a, b)| a.key == b.key)
    }
}
impl Eq for Excludes {}

/// Why a pattern from fleet.toml (or the wire) is refused, if it is.
pub fn check(raw: &str) -> std::result::Result<(), String> {
    pattern::parse(raw).map(|_| ())
}

fn parsed(raw: &str) -> Result<pattern::Parsed> {
    pattern::parse(raw).map_err(|why| Error::Config(format!("exclude pattern {raw:?}: {why}")))
}

/// The root exclude that leaves nothing of the unit at `prefix` (its path
/// below the root) to sync, if one does: it matches the unit or one of its
/// ancestors, root-relatively, or everything inside it (`cache/**` for unit
/// `cache`). The unit is taken to be a directory.
pub fn covering(root_excludes: &[String], prefix: &str) -> Result<Option<String>> {
    if prefix.is_empty() {
        return Ok(None);
    }
    for raw in root_excludes {
        let p = parsed(raw)?;
        let mut alone = Excludes::default();
        alone.add(p.clone());
        if alone.path_excluded(prefix, true) || p.rebase(prefix) == pattern::Rebased::Covers {
            return Ok(Some(raw.clone()));
        }
    }
    Ok(None)
}

impl Excludes {
    pub fn new<S: AsRef<str>>(patterns: &[S]) -> Result<Self> {
        let mut e = Self::default();
        for p in patterns {
            e.push(p.as_ref())?;
        }
        Ok(e)
    }

    /// The built-ins plus a root's own excludes, applied at the root.
    pub fn for_root(root_excludes: &[String]) -> Result<Self> {
        Self::for_unit(root_excludes, "")
    }

    /// The built-ins plus a root's own excludes, as they apply inside the
    /// unit at `prefix`, the unit's path below its root (`""` at the root).
    /// A pattern with a `/` is relative to the root and is rewritten for the
    /// unit (review P3, `pattern/rebase.rs`); names and `**/` patterns match
    /// at any depth as written. A unit that a pattern [`covering`] excludes
    /// whole gets `/**`: nothing inside it is ever walked or sent.
    pub fn for_unit(root_excludes: &[String], prefix: &str) -> Result<Self> {
        let mut e = Self::new(BUILTIN)?;
        if covering(root_excludes, prefix)?.is_some() {
            e.add(pattern::everything());
            return Ok(e);
        }
        for raw in root_excludes {
            let p = parsed(raw)?;
            match p.rebase(prefix) {
                pattern::Rebased::Same => e.add(p),
                pattern::Rebased::Within(ps) => ps.into_iter().for_each(|q| e.add(q)),
                pattern::Rebased::Covers => unreachable!("covering() checked every pattern"),
            }
        }
        Ok(e)
    }

    /// Add one pattern, or say why it is refused.
    pub fn push(&mut self, raw: &str) -> Result<()> {
        self.add(parsed(raw)?);
        Ok(())
    }

    /// Exclude one exact path relative to the unit root (a repo dir, a
    /// worktree checkout, an `except`). Every character is literal.
    pub fn push_exact(&mut self, rel: &str, is_dir: bool) {
        let p = pattern::literal(rel, is_dir);
        if !p.is_empty() {
            self.add(p);
        }
    }

    fn add(&mut self, p: pattern::Parsed) {
        let glob = p.glob();
        let key = format!(
            "{}{glob}{}",
            if p.anchored { "/" } else { "" },
            if p.dir_only { "/" } else { "" }
        );
        if self.patterns.iter().any(|q| q.key == key) {
            return;
        }
        self.patterns.push(Pattern {
            key,
            glob: compile(&glob),
            anchored: p.anchored,
            dir_only: p.dir_only,
        });
        self.compiled = OnceLock::new();
    }

    fn compiled(&self) -> &Compiled {
        self.compiled.get_or_init(|| {
            let mut sets: [GlobSetBuilder; 4] = std::array::from_fn(|_| GlobSetBuilder::new());
            for p in &self.patterns {
                let i = usize::from(p.anchored) * 2 + usize::from(p.dir_only);
                sets[i].add(p.glob.clone());
            }
            let [free, free_dir, rooted, rooted_dir] = sets.map(|b| {
                // Each glob compiled alone when it was rendered; a set of them
                // fails only past regex size limits, which a few hundred
                // literal paths and short globs do not approach.
                b.build().expect("a set of valid globs builds")
            });
            Compiled {
                free,
                free_dir,
                rooted,
                rooted_dir,
            }
        })
    }

    /// Whether this one entry matches, ignoring its ancestors.
    pub fn matches(&self, rel: &str, is_dir: bool) -> bool {
        if self.patterns.is_empty() {
            return false;
        }
        let c = self.compiled();
        let path = Candidate::new(rel);
        let name = Candidate::new(rel.rsplit('/').next().unwrap_or(rel));
        let free = |s: &GlobSet| s.is_match_candidate(&path) || s.is_match_candidate(&name);
        free(&c.free)
            || c.rooted.is_match_candidate(&path)
            || (is_dir && (free(&c.free_dir) || c.rooted_dir.is_match_candidate(&path)))
    }

    /// Whether a path is excluded: it, or any directory above it, matches.
    pub fn path_excluded(&self, rel: &str, is_dir: bool) -> bool {
        let rel = rel.trim_end_matches('/');
        let mut end = 0;
        while let Some(i) = rel[end..].find('/') {
            if self.matches(&rel[..end + i], true) {
                return true;
            }
            end += i + 1;
        }
        self.matches(rel, is_dir)
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }
}

/// A rendered pattern as a globset glob. Rendering only produces text the
/// parser has already validated (and escapes every literal), so this cannot
/// fail; `exclude_tests.rs` renders every ASCII character to hold it to that.
fn compile(text: &str) -> globset::Glob {
    GlobBuilder::new(text)
        .literal_separator(true)
        .backslash_escape(true)
        .build()
        .expect("rendered globs are valid")
}

#[cfg(test)]
#[path = "exclude_tests.rs"]
mod tests;
