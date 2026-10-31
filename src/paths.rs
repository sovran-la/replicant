//! The one path normalizer, and the relative-path helpers everything else uses.
//!
//! PLAN.md, "Path normalization": `subscribe`, `unsubscribe`, `approve` and
//! `allow` all go through [`Normalizer`], so a path means the same thing on
//! both ends. A normalized path is relative to a home dir (`work/tundra`), uses
//! `/`, has no `.`/`..`/empty components, and lies under a configured root.
//!
//! The rules, in order:
//!
//! 1. `~` and `~/x` → `x`
//! 2. absolute under the *leaf's* home dir → stripped
//! 3. absolute under *home's* own home dir → stripped (muscle memory on home)
//! 4. trailing slashes dropped, duplicates collapsed, `..` refused
//! 5. anything not under a configured root → refused, with the roots listed
//!
//! A bare relative path is taken as already relative to the home dir, which is
//! what `approve`'s allow prompt asks for. The CLI absolutizes relative paths
//! typed at `subscribe`/`unsubscribe` against the working directory before
//! they get here, because that is what a relative path means in a shell.

use crate::{Error, Result};
use std::path::Path;

/// Normalizes paths typed on (or sent from) one particular leaf.
#[derive(Debug, Clone, Copy)]
pub struct Normalizer<'a> {
    /// The home dir of the host whose paths these are.
    pub leaf_home: &'a Path,
    /// Home's own home dir.
    pub home_home: &'a Path,
    /// The roots home serves, relative to its home dir.
    pub roots: &'a [String],
}

impl Normalizer<'_> {
    /// Normalize a list, preserving first-seen order and collapsing duplicates.
    ///
    /// Every bad path is reported, not just the first, so one round trip fixes
    /// a whole command line.
    pub fn normalize(&self, raw: &[String]) -> Result<Vec<String>> {
        let mut out: Vec<String> = Vec::new();
        let mut problems = Vec::new();
        for r in raw {
            match self.one(r) {
                Ok(p) if !out.contains(&p) => out.push(p),
                Ok(_) => {}
                Err(why) => problems.push(format!("{r}: {why}")),
            }
        }
        if !problems.is_empty() {
            return Err(Error::Path(problems.join("; ")));
        }
        Ok(out)
    }

    /// Normalize one path, or say why not.
    pub fn one(&self, raw: &str) -> std::result::Result<String, String> {
        let raw = raw.trim();
        let rel: &str = if raw == "~" {
            ""
        } else if let Some(rest) = raw.strip_prefix("~/") {
            rest
        } else if raw.starts_with('~') {
            return Err("`~user` paths are not supported; use ~/ or an absolute path".into());
        } else if raw.starts_with('/') {
            strip_home(raw, self.leaf_home, self.home_home)?
        } else {
            raw
        };

        let mut parts = Vec::new();
        for c in rel.split('/') {
            match c {
                "" | "." => {}
                ".." => return Err("`..` is not allowed".into()),
                c => parts.push(c),
            }
        }
        if parts.is_empty() {
            return Err("that is the home dir itself, not a path under a root".into());
        }
        let rel = parts.join("/");
        if !self.roots.iter().any(|root| is_within(&rel, root)) {
            return Err(format!(
                "not under a root home serves (roots: {})",
                if self.roots.is_empty() {
                    "none configured".into()
                } else {
                    self.roots.join(", ")
                }
            ));
        }
        Ok(rel)
    }
}

/// Strip whichever home dir an absolute path is under, the longer one first.
fn strip_home<'p>(raw: &'p str, leaf: &Path, home: &Path) -> std::result::Result<&'p str, String> {
    let mut candidates = [leaf, home];
    candidates.sort_by_key(|p| std::cmp::Reverse(p.as_os_str().len()));
    for base in candidates {
        let base = base.to_str().unwrap_or("").trim_end_matches('/');
        if base.is_empty() {
            continue;
        }
        if raw == base {
            return Ok("");
        }
        if let Some(rest) = raw.strip_prefix(base).and_then(|r| r.strip_prefix('/')) {
            return Ok(rest);
        }
    }
    Err(format!(
        "absolute, but under neither this host's home dir ({}) nor home's ({})",
        leaf.display(),
        home.display()
    ))
}

/// Whether `child` is `parent` or lies beneath it, component-wise.
///
/// `work/tundra` is within `work`; `work2` is not.
pub fn is_within(child: &str, parent: &str) -> bool {
    child == parent
        || (child.len() > parent.len()
            && child.starts_with(parent)
            && child.as_bytes()[parent.len()] == b'/')
}

/// `child` relative to `parent`, when it is within it. `""` for equality.
pub fn relative_to<'c>(child: &'c str, parent: &str) -> Option<&'c str> {
    if child == parent {
        Some("")
    } else if is_within(child, parent) {
        Some(&child[parent.len() + 1..])
    } else {
        None
    }
}

/// Whether a raw path has a `..` component.
pub fn has_dotdot(raw: &str) -> bool {
    raw.split('/').any(|c| c == "..")
}

/// Check a path that claims to be normalized already (config, wire).
pub fn check_rel(rel: &str) -> std::result::Result<(), &'static str> {
    if rel.is_empty() {
        return Err("is empty");
    }
    if rel.starts_with('/') {
        return Err("must be relative to the home dir");
    }
    if rel
        .split('/')
        .any(|c| c.is_empty() || c == "." || c == "..")
    {
        return Err("must have no empty, `.` or `..` components and no trailing slash");
    }
    Ok(())
}

/// Drop every path that lies within another in the list, keeping order.
///
/// Returns `(kept, absorbed)`: subscribing `work` after `work/tundra` keeps
/// `work` and reports `work/tundra` as absorbed.
pub fn collapse(list: &[String]) -> (Vec<String>, Vec<String>) {
    let mut kept = Vec::new();
    let mut absorbed = Vec::new();
    for p in list {
        if kept.contains(p) || absorbed.contains(p) {
            continue;
        }
        if list.iter().any(|q| q != p && is_within(p, q)) {
            absorbed.push(p.clone());
        } else {
            kept.push(p.clone());
        }
    }
    (kept, absorbed)
}

#[cfg(test)]
#[path = "paths_tests.rs"]
mod tests;
