//! The receiving side of a transfer, writing into one unit: home on a
//! push, the leaf on a pull. Every write of a synced path is here.
//!
//! A file is written to `.replicant-tmp-<name>` in its own directory, given
//! the source's mode and mtime (to the nanosecond, so the state hash agrees
//! afterwards), fsynced, and only then renamed over its target: a target
//! file is never torn. Links are made under a temporary name and renamed
//! the same way, and never followed. Directories are created owner-writable
//! and get their final mode last ([`Receiver::dir_modes`]), so a read-only
//! source directory can still be filled.
//!
//! Every path is checked before it is touched: inside the unit, through
//! real directories only (a symlinked directory inside the unit is never
//! written through), and not excluded by home's policy when home receives.
//! With a [`Backup`] (home, on a push), whatever a write replaces or a
//! deletion removes is renamed into the backup first. Without one (the leaf,
//! on a pull) nothing would bring it back, so the receiver is given the
//! target's manifest ([`Receiver::expecting`]) and refuses to overwrite or
//! delete a path that is no longer what the diff saw.

use super::backup::{Backup, remove_tree};
use super::manifest::Manifest;
#[cfg(test)]
use super::manifest::{Entry, EntryKind};
use crate::exclude::Excludes;
use crate::{Error, Result, paths};
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct Receiver<'a> {
    root: PathBuf,
    is_file: bool,
    policy: Option<&'a Excludes>,
    backup: Option<Backup>,
    /// The target as the diff saw it, if this receiver is held to it.
    expect: Option<&'a Manifest>,
    /// Directories written into (fsynced at the end).
    touched: Mutex<HashSet<PathBuf>>,
}

pub(super) fn bad(rel: &str, why: impl std::fmt::Display) -> Error {
    Error::Transfer(format!("{rel:?}: {why}"))
}

impl<'a> Receiver<'a> {
    /// A receiver for the unit at `root`. A directory unit's symlinked root
    /// is the directory it names (review P4); a missing one is created.
    pub fn open(
        root: &Path,
        is_file: bool,
        policy: Option<&'a Excludes>,
        backup: Option<Backup>,
    ) -> Result<Self> {
        let root = match fs::symlink_metadata(root) {
            Ok(m) if m.file_type().is_symlink() && !is_file => fs::canonicalize(root)?,
            Ok(_) => root.to_path_buf(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let dir = if is_file { root.parent() } else { Some(root) };
                if let Some(d) = dir {
                    fs::create_dir_all(d)?;
                }
                root.to_path_buf()
            }
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            root,
            is_file,
            policy,
            backup,
            expect: None,
            touched: Mutex::default(),
        })
    }

    /// Hold this receiver to `target`, the manifest the change list was
    /// diffed against ([`guard`]).
    pub fn expecting(mut self, target: &'a Manifest) -> Self {
        self.expect = Some(target);
        self
    }

    /// `abs` (`rel`) is about to be overwritten or deleted: it must still be
    /// what the diff saw, or nothing, else the unit fails rather than lose
    /// what was written there since.
    pub(super) fn guard(&self, abs: &Path, rel: &str) -> Result<()> {
        let Some(target) = self.expect else {
            return Ok(());
        };
        guard::still(target, abs, rel)
    }

    /// The absolute path of unit-relative `rel`, once it is known safe to
    /// write: a real relative path (`""` only for a single-file unit),
    /// allowed by policy, under directories that are real directories.
    pub(super) fn path(&self, rel: &str, is_dir: bool) -> Result<PathBuf> {
        self.checked(rel, is_dir, true)
    }

    /// [`Self::path`]; `policy` false only for a temporary name, which the
    /// built-in excludes leave out and only [`Self::clean`] touches.
    fn checked(&self, rel: &str, is_dir: bool, policy: bool) -> Result<PathBuf> {
        if rel.is_empty() {
            return match self.is_file {
                true => Ok(self.root.clone()),
                false => Err(bad(rel, "is the unit's own directory")),
            };
        }
        if self.is_file {
            return Err(bad(rel, "is inside a unit that is a single file"));
        }
        paths::check_rel(rel).map_err(|why| bad(rel, why))?;
        if policy && self.policy.is_some_and(|p| p.path_excluded(rel, is_dir)) {
            return Err(bad(rel, "is excluded on home, which does not accept it"));
        }
        let abs = self.root.join(rel);
        if let Some(parent) = abs.parent() {
            self.real_dir(parent)?;
        }
        Ok(abs)
    }

    /// `dir` and every directory between it and the root are directories,
    /// not links to one. Looked at every time: what was a directory a moment
    /// ago may be a link now, by a frame or by anyone else on this machine.
    fn real_dir(&self, dir: &Path) -> Result<()> {
        let mut d = dir;
        while d != self.root {
            let m = fs::symlink_metadata(d).map_err(|e| {
                Error::Transfer(format!("{} cannot be written into: {e}", d.display()))
            })?;
            if !m.is_dir() {
                return Err(Error::Transfer(format!(
                    "{} is not a directory (a link is never written through)",
                    d.display()
                )));
            }
            d = match d.parent() {
                Some(p) if p.starts_with(&self.root) => p,
                _ => {
                    return Err(Error::Transfer(format!(
                        "{} is outside the unit",
                        d.display()
                    )));
                }
            };
        }
        Ok(())
    }

    pub(super) fn touch(&self, abs: &Path) {
        if let Some(p) = abs.parent() {
            let mut t = self.touched.lock().unwrap_or_else(|e| e.into_inner());
            t.insert(p.to_path_buf());
        }
    }

    /// The backup's name for unit-relative `rel`: a single-file unit's root
    /// is kept under its file name, as the list operations name it.
    fn backup_rel(&self, rel: &str) -> String {
        match (rel.is_empty(), self.root.file_name()) {
            (true, Some(n)) => n.to_string_lossy().into_owned(),
            _ => rel.to_string(),
        }
    }

    /// Note what directory `rel` was, on home, before it changes.
    pub(super) fn note_dir(&self, rel: &str, mode: Option<u32>) -> Result<()> {
        match &self.backup {
            Some(b) => b.note_dir(rel, mode),
            None => Ok(()),
        }
    }

    /// Move whatever is at `abs` out of the way: into the backup on home,
    /// else gone.
    pub(super) fn displace(&self, abs: &Path, rel: &str) -> Result<()> {
        match &self.backup {
            Some(b) => b.keep(abs, &self.backup_rel(rel)),
            None => remove_tree(abs).map_err(|why| bad(rel, why)),
        }
    }
}

#[path = "receive_tree.rs"]
mod tree;

#[path = "receive_file.rs"]
mod file;
pub use file::Incoming;

#[path = "receive_guard.rs"]
mod guard;

#[cfg(test)]
#[path = "receive_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "receive_backup_tests.rs"]
mod backup_tests;

#[cfg(test)]
#[path = "receive_guard_tests.rs"]
mod guard_tests;
