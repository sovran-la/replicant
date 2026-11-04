//! Discovery: the repos, worktrees and loose files under one path.
//!
//! A **repo** is a directory with a `.git` *directory*. Discovery does not
//! descend into one: the repo is a unit of its own, synced and hashed whole.
//!
//! A **worktree** is a directory whose `.git` is a *file* whose `gitdir:`
//! points into some repo's `.git/worktrees/`. PLAN.md, "Exclusions": worktrees
//! are machine-local and never synced; the checkout is excluded here and the
//! parent's `.git/worktrees/` by [`crate::exclude::BUILTIN`]. Any other `.git`
//! file (a submodule's, a `--separate-git-dir` repo's) is not a worktree and
//! its directory is walked as ordinary content.
//!
//! Everything else is **loose**: a file, symlink or directory that belongs to
//! no repo, recorded with enough metadata for the manifest in [`crate::state`].
//!
//! Symlinks inside the tree are never followed: they are entries, and a
//! transfer makes them as links. The one exception is a subscription's base
//! ([`discover_base`], review P4): `~/work2 → /Volumes/data/work2` is the
//! directory it names, and a dangling one is missing. The walk is parallel
//! ([`walk`]); every list in a [`Discovery`] is sorted, so the result is the
//! same run to run.
//!
//! **A missing base is not an empty one** (review F4). [`discover`] reports a
//! base that does not exist as `exists: false`, and its callers must not
//! read that as "nothing here": on its own side a missing subscription base
//! is an error naming the path and the host, unless no baseline lies under
//! it (never synced: the first pull or push, the one legitimate empty case).
//! The leaf holds the baselines and applies the rule to both sides
//! (`plan::bases`); home reports its missing bases in the snapshot, and a
//! missing *root* on home is always an error from home.

use crate::Result;
use crate::exclude::Excludes;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

mod ignored;
mod walk;

pub use ignored::ignored;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    File,
    Symlink,
    Dir,
}

/// One loose entry, relative to the discovered base.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Entry {
    pub rel: String,
    pub kind: Kind,
    pub size: u64,
    /// Seconds. Directories record 0: their mtime moves whenever a child does
    /// and says nothing a child's entry does not.
    pub mtime: i64,
    pub mode: u32,
    pub link: Option<String>,
}

/// What lies under one base path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovery {
    /// Whether the base exists at all.
    pub exists: bool,
    /// Repo directories, relative to the base; `""` when the base is a repo.
    pub repos: Vec<String>,
    /// Worktree checkouts, relative to the base.
    pub worktrees: Vec<String>,
    /// Loose entries, sorted. A base that is a single file is one entry, `""`.
    pub loose: Vec<Entry>,
}

/// Discover under `base`, skipping anything `ex` excludes (relative to base).
/// A `base` that does not exist is `exists: false`, never an error here: see
/// the module doc for what callers must make of it.
pub fn discover(base: &Path, ex: &Excludes) -> Result<Discovery> {
    let mut d = Discovery::default();
    let meta = match fs::symlink_metadata(base) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(d),
        Err(e) => return Err(e.into()),
    };
    d.exists = true;
    if is_special(&meta) {
        return Ok(d);
    }
    if !meta.is_dir() {
        d.loose.push(entry(base, "", &meta)?);
        return Ok(d);
    }
    match git_marker(base) {
        GitMarker::Repo => d.repos.push(String::new()),
        GitMarker::Worktree => d.worktrees.push(String::new()),
        GitMarker::None | GitMarker::OtherFile => walk::walk(base, ex, &mut d)?,
    }
    d.repos.sort();
    d.worktrees.sort();
    d.loose.sort();
    Ok(d)
}

/// [`discover`] for a subscription base, following it if it is a symlink
/// (review P4): the most likely `--as` layout is a link to an external
/// volume, and read as a link it would be one loose file. A dangling link is
/// `exists: false`, so the missing-base rule applies to an unmounted volume.
/// Only the base is followed; links inside the tree stay entries.
pub fn discover_base(base: &Path, ex: &Excludes) -> Result<Discovery> {
    match fs::symlink_metadata(base) {
        Ok(m) if m.file_type().is_symlink() => match fs::canonicalize(base) {
            Ok(target) => discover(&target, ex),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Discovery::default()),
            Err(e) => Err(e.into()),
        },
        _ => discover(base, ex),
    }
}

/// A socket, FIFO or device: nothing a sync carries. It is never listed (not
/// by discovery, a state hash or a manifest, so no diff asks for one and no
/// verification counts one) and never opened: opening a FIFO blocks.
pub fn is_special(meta: &fs::Metadata) -> bool {
    let ft = meta.file_type();
    !(ft.is_file() || ft.is_symlink() || ft.is_dir())
}

/// What a directory's `.git` says it is.
#[derive(Debug, PartialEq, Eq)]
pub enum GitMarker {
    None,
    Repo,
    Worktree,
    /// A `.git` file that is not a worktree (submodule, separate git dir).
    OtherFile,
}

/// A directory is a repo when its `.git` is a directory holding `HEAD`, as
/// git itself requires: a `.git` still arriving (an interrupted pull of a
/// new repo, before its `HEAD`) is plain content until it is whole, rather
/// than a repo `git status` cannot read, which would fail every plan and
/// so the resume that completes it.
pub fn git_marker(dir: &Path) -> GitMarker {
    let dot = dir.join(".git");
    match fs::symlink_metadata(&dot) {
        Ok(m) if m.is_dir() && fs::symlink_metadata(dot.join("HEAD")).is_ok() => GitMarker::Repo,
        Ok(m) if m.is_dir() => GitMarker::None,
        Ok(m) if m.is_file() => match fs::read_to_string(&dot) {
            Ok(text) if points_into_worktrees(&text, dir) => GitMarker::Worktree,
            _ => GitMarker::OtherFile,
        },
        _ => GitMarker::None,
    }
}

/// Whether a `.git` file's `gitdir:` lands in some repo's `.git/worktrees/`.
fn points_into_worktrees(text: &str, dir: &Path) -> bool {
    let Some(target) = text.lines().find_map(|l| l.strip_prefix("gitdir:")) else {
        return false;
    };
    let target = Path::new(target.trim());
    let resolved = if target.is_absolute() {
        target.to_path_buf()
    } else {
        dir.join(target)
    };
    let comps: Vec<_> = resolved
        .components()
        .map(|c| c.as_os_str().to_owned())
        .collect();
    comps
        .windows(3)
        .any(|w| w[0] == ".git" && w[1] == "worktrees" && !w[2].is_empty())
}

fn entry(abs: &Path, rel: &str, meta: &fs::Metadata) -> Result<Entry> {
    let ft = meta.file_type();
    let (kind, link) = if ft.is_symlink() {
        (
            Kind::Symlink,
            Some(fs::read_link(abs)?.to_string_lossy().into_owned()),
        )
    } else if ft.is_dir() {
        (Kind::Dir, None)
    } else {
        (Kind::File, None)
    };
    Ok(Entry {
        rel: rel.to_string(),
        kind,
        size: if kind == Kind::File { meta.len() } else { 0 },
        mtime: if kind == Kind::File { meta.mtime() } else { 0 },
        mode: meta.permissions().mode() & 0o7777,
        link,
    })
}

/// Every path under `base` including inside repos (but not `.git/objects`,
/// whose names are hex and cannot collide), for case-collision checks.
/// `base`, a unit's root, is followed if it is a symlink, as discovery and
/// git do (review P4); nothing below it is.
pub fn all_names(base: &Path, ex: &Excludes) -> Result<Vec<String>> {
    let mut out = Vec::new();
    if fs::metadata(base).map(|m| m.is_dir()).unwrap_or(false) {
        walk_names(base, "", ex, &mut out)?;
    }
    Ok(out)
}

fn walk_names(abs: &Path, rel: &str, ex: &Excludes, out: &mut Vec<String>) -> Result<()> {
    if rel.ends_with(".git/objects") || rel == ".git/objects" {
        return Ok(());
    }
    if !rel.is_empty() && git_marker(abs) == GitMarker::Worktree {
        return Ok(());
    }
    for item in fs::read_dir(abs)? {
        let item = item?;
        let Some(name) = item.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let child_rel = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        let ft = item.file_type()?;
        let is_dir = ft.is_dir();
        if ex.matches(&child_rel, is_dir) || !(is_dir || ft.is_file() || ft.is_symlink()) {
            continue;
        }
        if is_dir {
            walk_names(&item.path(), &child_rel, ex, out)?;
        }
        out.push(child_rel);
    }
    Ok(())
}

/// Pairs of paths that differ only by case. PLAN.md guardrail 4: these would be
/// silently merged by a case-insensitive target.
///
/// Each pair is ordered and the list is sorted, so the result does not depend
/// on directory read order (ext4 and APFS disagree about it).
pub fn case_collisions<'a>(paths: impl IntoIterator<Item = &'a str>) -> Vec<(String, String)> {
    let mut seen: std::collections::BTreeMap<String, String> = Default::default();
    let mut out = Vec::new();
    for p in paths {
        let folded = p.to_lowercase();
        match seen.get(&folded) {
            Some(first) if first != p => {
                let (a, b) = if first.as_str() < p {
                    (first.clone(), p.to_string())
                } else {
                    (p.to_string(), first.clone())
                };
                out.push((a, b));
            }
            Some(_) => {}
            None => {
                seen.insert(folded, p.to_string());
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
#[path = "repos_tests.rs"]
mod tests;
