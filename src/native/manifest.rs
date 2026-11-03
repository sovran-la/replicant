//! One side of one unit, listed: every entry the transfer would move, with
//! the metadata the diff compares.
//!
//! The walk applies the unit's [`Excludes`] as it goes, exactly as the
//! state hash's walks do: an excluded entry is not listed and an excluded
//! directory is not descended. The excludes already hold everything a unit
//! leaves out (root excludes rewritten for the unit, carve-outs, worktree
//! checkouts, a loose unit's repos, both sides' git-ignored paths), so
//! nothing here knows about git. Symlinks are entries, never followed. The
//! unit's own root is followed when it is a directory unit's symlinked
//! base (review P4); a single-file unit's root is an entry, `""`.
//!
//! The receiver's temporary names are never listed; they are collected in
//! [`Manifest::temps`] so the next transfer can remove them. Sockets, FIFOs
//! and devices are never listed either ([`Manifest::skipped`] names them, for
//! the unit's notes), so the diff never asks for one. A single-file unit's
//! temporary lies beside it, not under it, and is collected just the same.

use super::{is_temp, temp_name};
use crate::exclude::Excludes;
use crate::repos::is_special;
use crate::{Error, Result};
use ignore::{DirEntry, ParallelVisitor, ParallelVisitorBuilder, WalkBuilder, WalkState};
use serde::{Deserialize, Serialize};
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::sync::mpsc::{Sender, channel};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum EntryKind {
    #[serde(rename = "f")]
    File,
    #[serde(rename = "l")]
    Symlink,
    #[serde(rename = "d")]
    Dir,
}

/// One entry. Short field names: a loose unit can list 70k of these.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Entry {
    /// Relative to the unit; `""` is a single-file unit's root.
    #[serde(rename = "p")]
    pub rel: String,
    #[serde(rename = "k")]
    pub kind: EntryKind,
    /// Bytes, for a file; 0 otherwise.
    #[serde(rename = "s", default)]
    pub size: u64,
    /// Nanoseconds since the epoch, for a file; 0 otherwise. A directory's
    /// mtime moves whenever a child does, and a link's is not state.
    #[serde(rename = "m", default)]
    pub mtime_ns: i64,
    /// Permission bits (`& 0o7777`).
    #[serde(rename = "o")]
    pub mode: u32,
    /// A symlink's target, as written.
    #[serde(rename = "l", default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Dir
    }

    /// The entry for `abs`, called `rel`, from its `lstat`. A socket, FIFO
    /// or device has none: a sync does not carry it.
    pub fn of(abs: &Path, rel: &str, meta: &fs::Metadata) -> Result<Self> {
        if is_special(meta) {
            return Err(Error::Transfer(format!(
                "{rel:?} is a socket, FIFO or device, which a sync neither carries nor replaces"
            )));
        }
        let ft = meta.file_type();
        let (kind, link) = if ft.is_symlink() {
            let target = fs::read_link(abs)?;
            (
                EntryKind::Symlink,
                Some(target.to_string_lossy().into_owned()),
            )
        } else if ft.is_dir() {
            (EntryKind::Dir, None)
        } else {
            (EntryKind::File, None)
        };
        let file = kind == EntryKind::File;
        Ok(Entry {
            rel: rel.to_string(),
            kind,
            size: if file { meta.len() } else { 0 },
            mtime_ns: if file { mtime_ns(meta) } else { 0 },
            mode: meta.permissions().mode() & 0o7777,
            link,
        })
    }
}

/// A path's mtime in nanoseconds since the epoch.
pub fn mtime_ns(meta: &fs::Metadata) -> i64 {
    meta.mtime()
        .saturating_mul(1_000_000_000)
        .saturating_add(meta.mtime_nsec())
}

/// One side of a unit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Sorted by path, so parents come before their children.
    #[serde(rename = "e")]
    pub entries: Vec<Entry>,
    /// The receiver's temporary files found, relative to the unit.
    #[serde(rename = "t", default)]
    pub temps: Vec<String>,
    /// Sockets, FIFOs and devices found, relative to the unit: not listed,
    /// not synced, not touched. Sorted.
    #[serde(rename = "x", default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
}

impl Manifest {
    /// Walk the unit at `root`. `is_file`: a single-file unit, whose root is
    /// an entry and never followed. A root that does not exist lists nothing.
    pub fn walk(root: &Path, ex: &Excludes, is_file: bool) -> Result<Self> {
        let mut m = Self::walked(root, ex, is_file)?;
        if is_file && let Some(t) = beside(root) {
            m.temps.push(t);
        }
        Ok(m)
    }

    fn walked(root: &Path, ex: &Excludes, is_file: bool) -> Result<Self> {
        let meta = match fs::symlink_metadata(root) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        let base = match (meta.file_type().is_symlink(), is_file) {
            // A symlinked base is the directory it names, and a dangling
            // one is missing, as discovery reads it (`repos::discover_base`).
            (true, false) => match fs::canonicalize(root) {
                Ok(t) if t.is_dir() => t,
                Ok(t) => return Self::one(&t, &fs::symlink_metadata(&t)?),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(Self::default());
                }
                Err(e) => return Err(e.into()),
            },
            _ if meta.is_dir() => root.to_path_buf(),
            _ => return Self::one(root, &meta),
        };
        let mut m = walk_dir(&base, ex)?;
        m.entries.sort();
        m.temps.sort();
        m.skipped.sort();
        Ok(m)
    }

    fn one(root: &Path, meta: &fs::Metadata) -> Result<Self> {
        if is_special(meta) {
            return Ok(Self {
                skipped: vec![String::new()],
                ..Self::default()
            });
        }
        Ok(Self {
            entries: vec![Entry::of(root, "", meta)?],
            ..Self::default()
        })
    }

    pub fn get(&self, rel: &str) -> Option<&Entry> {
        self.entries
            .binary_search_by(|e| e.rel.as_str().cmp(rel))
            .ok()
            .map(|i| &self.entries[i])
    }

    /// Files and links, and their bytes.
    pub fn totals(&self) -> (u64, u64) {
        let files = self.entries.iter().filter(|e| !e.is_dir());
        files.fold((0, 0), |(n, b), e| (n + 1, b + e.size))
    }
}

/// A single-file unit's temporary, a dead transfer's, if it is there: named as
/// it is in the unit's directory, since it is in no directory of the unit's.
fn beside(root: &Path) -> Option<String> {
    let t = temp_name(root.file_name()?.to_str()?);
    let found = fs::symlink_metadata(root.with_file_name(&t));
    found
        .is_ok_and(|m| !m.is_dir() && !is_special(&m))
        .then_some(t)
}

#[derive(Default)]
struct Found {
    entries: Vec<Entry>,
    temps: Vec<String>,
    skipped: Vec<String>,
    error: Option<Error>,
}

struct Visitor<'a> {
    base: &'a Path,
    ex: &'a Excludes,
    found: Found,
    tx: Sender<Found>,
}

impl Drop for Visitor<'_> {
    fn drop(&mut self) {
        let _ = self.tx.send(std::mem::take(&mut self.found));
    }
}

impl ParallelVisitor for Visitor<'_> {
    fn visit(&mut self, item: std::result::Result<DirEntry, ignore::Error>) -> WalkState {
        match self.step(item) {
            Ok(state) => state,
            Err(e) => {
                self.found.error.get_or_insert(e);
                WalkState::Quit
            }
        }
    }
}

impl Visitor<'_> {
    fn step(&mut self, item: std::result::Result<DirEntry, ignore::Error>) -> Result<WalkState> {
        let item = item.map_err(|e| match e.into_io_error() {
            Some(io) => Error::Io(io),
            None => Error::Io(std::io::Error::other("directory walk failed")),
        })?;
        if item.depth() == 0 {
            return Ok(WalkState::Continue);
        }
        let abs = item.path();
        // Not UTF-8: skipped and not descended, as discovery does.
        let Some(rel) = abs.strip_prefix(self.base).ok().and_then(Path::to_str) else {
            return Ok(WalkState::Skip);
        };
        let meta = fs::symlink_metadata(abs)?;
        let name = rel.rsplit('/').next().unwrap_or(rel);
        if is_temp(name) && !meta.is_dir() && !is_special(&meta) {
            self.found.temps.push(rel.to_string());
            return Ok(WalkState::Skip);
        }
        if self.ex.matches(rel, meta.is_dir()) {
            return Ok(WalkState::Skip);
        }
        if is_special(&meta) {
            self.found.skipped.push(rel.to_string());
            return Ok(WalkState::Skip);
        }
        self.found.entries.push(Entry::of(abs, rel, &meta)?);
        Ok(WalkState::Continue)
    }
}

struct Builder<'a> {
    base: &'a Path,
    ex: &'a Excludes,
    tx: Sender<Found>,
}

impl<'s> ParallelVisitorBuilder<'s> for Builder<'s> {
    fn build(&mut self) -> Box<dyn ParallelVisitor + 's> {
        Box::new(Visitor {
            base: self.base,
            ex: self.ex,
            found: Found::default(),
            tx: self.tx.clone(),
        })
    }
}

/// Every entry under the directory `base`, unsorted.
fn walk_dir(base: &Path, ex: &Excludes) -> Result<Manifest> {
    let (tx, rx) = channel();
    let mut builder = Builder { base, ex, tx };
    WalkBuilder::new(base)
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .ignore(false)
        .parents(false)
        .require_git(false)
        .follow_links(false)
        .threads(crate::state::workers())
        .build_parallel()
        .visit(&mut builder);
    drop(builder);
    let mut m = Manifest::default();
    let mut error = None;
    for f in rx {
        m.entries.extend(f.entries);
        m.temps.extend(f.temps);
        m.skipped.extend(f.skipped);
        error = error.or(f.error);
    }
    match error {
        Some(e) => Err(e),
        None => Ok(m),
    }
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
pub(crate) mod tests;
