//! What git ignores inside a repo, which does not sync (PLAN.md rev 7,
//! "State hash").
//!
//! Only the repo's own `.gitignore` files count, nested ones included: never
//! the user's global gitignore (it differs per machine) and never
//! `.git/info/exclude`. The same rule decides what replicant walks, what the
//! hash sees, and what rsync is told to leave alone on *both* ends, so an
//! ignored `.env.local` on the target is never deleted for being absent on
//! the source.
//!
//! **Why not rsync's `--filter=':- .gitignore'`.** Neither macOS's openrsync
//! nor rsync 3.4.1 reads a `!negation` the way git does: both excluded
//! `keep.log` under `*.log` / `!keep.log` (probe, `docs/v0.2-prep-notes.md`).
//! So replicant lists the ignored paths itself and hands rsync one anchored
//! literal per path ([`crate::exclude::Excludes::push_exact`]).
//!
//! **Why not `git ls-files --ignored --directory`.** It collapses a directory
//! whose *current contents* happen to be all ignored (`sub/deep/` holding only
//! `secret.txt`). Unioned with the other side's list, that would hide the
//! other side's new, unignored WIP in `sub/deep/`. The list here is decided by
//! rules alone: a directory appears as `dir/` only when a rule ignores the
//! directory itself, so both sides' lists can be unioned safely.
//!
//! Matching is git's: the `ignore` crate's gitignore matcher, deepest
//! `.gitignore` first, last matching line wins, an ignored directory is never
//! descended (so nothing inside it can be re-included), case-insensitive when
//! the repo's `core.ignorecase` is set (macOS's default, and `.git/config`
//! syncs, so both sides read the same value). A nested repo or submodule (any
//! `.git` below the top) is matched as a directory and not descended: git
//! does not apply the parent's rules inside it. `repos/ignored_tests.rs` holds
//! the list against git itself.

use crate::Result;
use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::fs;
use std::path::Path;

/// The paths git ignores in the repo at `repo`, relative to it, sorted. A
/// directory ignored by a rule is one entry ending in `/`; nothing inside it
/// is listed. `.git` itself is never listed.
pub fn ignored(repo: &Path) -> Result<Vec<String>> {
    let icase = ignore_case(repo);
    let mut out = Vec::new();
    let mut stack = Vec::new();
    walk(repo, "", icase, &mut stack, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk(
    abs: &Path,
    rel: &str,
    icase: bool,
    stack: &mut Vec<Gitignore>,
    out: &mut Vec<String>,
) -> Result<()> {
    let own = load(abs, icase)?;
    let pushed = own.is_some();
    stack.extend(own);
    for item in fs::read_dir(abs)? {
        let item = item?;
        // A name that is not UTF-8 is skipped, as discovery skips it: no
        // relative path in this crate can hold it.
        let Some(name) = item.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if rel.is_empty() && name == ".git" {
            continue;
        }
        let child = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        let path = item.path();
        let is_dir = item.file_type()?.is_dir();
        if matched(stack, &path, is_dir) {
            out.push(if is_dir { format!("{child}/") } else { child });
        } else if is_dir && fs::symlink_metadata(path.join(".git")).is_err() {
            walk(&path, &child, icase, stack, out)?;
        }
    }
    if pushed {
        stack.pop();
    }
    Ok(())
}

/// The `.gitignore` in `dir`, if it holds one. git does not follow a
/// symlinked `.gitignore`, so neither does this. Lines that are not valid
/// patterns are dropped, as git drops them.
fn load(dir: &Path, icase: bool) -> Result<Option<Gitignore>> {
    let file = dir.join(".gitignore");
    match fs::symlink_metadata(&file) {
        Ok(m) if m.is_file() => {}
        Ok(_) => return Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let mut b = GitignoreBuilder::new(dir);
    b.case_insensitive(icase).map_err(std::io::Error::other)?;
    if let Some(e) = b.add(&file) {
        // A partial error is a bad line; an I/O error means nothing was read.
        if let Some(io) = e.io_error() {
            return Err(std::io::Error::new(io.kind(), io.to_string()).into());
        }
    }
    Ok(Some(b.build().map_err(std::io::Error::other)?))
}

/// git's precedence: the deepest `.gitignore` with an opinion decides.
fn matched(stack: &[Gitignore], path: &Path, is_dir: bool) -> bool {
    for g in stack.iter().rev() {
        match g.matched(path, is_dir) {
            Match::Ignore(_) => return true,
            Match::Whitelist(_) => return false,
            Match::None => {}
        }
    }
    false
}

/// The repo's own `core.ignorecase`, read from `.git/config` (includes are
/// not followed). Unset or unreadable is git's default, false.
pub(super) fn ignore_case(repo: &Path) -> bool {
    let Ok(text) = fs::read_to_string(repo.join(".git/config")) else {
        return false;
    };
    let mut in_core = false;
    let mut value = false;
    for line in text.lines() {
        let l = line.trim();
        if let Some(section) = l.strip_prefix('[') {
            let name = section.split(']').next().unwrap_or("").trim();
            in_core = name.eq_ignore_ascii_case("core");
            continue;
        }
        if !in_core {
            continue;
        }
        let (key, v) = l.split_once('=').unwrap_or((l, "true"));
        if key.trim().eq_ignore_ascii_case("ignorecase") {
            let v = v.split(['#', ';']).next().unwrap_or("").trim();
            value = ["true", "yes", "on", "1"]
                .iter()
                .any(|t| v.eq_ignore_ascii_case(t));
        }
    }
    value
}

#[cfg(test)]
#[path = "ignored_tests.rs"]
mod tests;
