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
//! deletion removes is renamed into the backup first.

use super::backup::{Backup, remove_tree};
use super::is_temp;
use super::manifest::{Entry, EntryKind};
use crate::exclude::Excludes;
use crate::{Error, Result, paths};
use std::collections::HashSet;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct Receiver<'a> {
    root: PathBuf,
    is_file: bool,
    policy: Option<&'a Excludes>,
    backup: Option<Backup>,
    /// Directories already checked to be real, and those written into
    /// (fsynced at the end).
    real: Mutex<HashSet<PathBuf>>,
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
            real: Mutex::default(),
            touched: Mutex::default(),
        })
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
    /// not links to one.
    fn real_dir(&self, dir: &Path) -> Result<()> {
        let mut real = self.real.lock().unwrap_or_else(|e| e.into_inner());
        let mut d = dir;
        let mut new = Vec::new();
        while d != self.root && !real.contains(d) {
            let m = fs::symlink_metadata(d).map_err(|e| {
                Error::Transfer(format!("{} cannot be written into: {e}", d.display()))
            })?;
            if !m.is_dir() {
                return Err(Error::Transfer(format!(
                    "{} is not a directory (a link is never written through)",
                    d.display()
                )));
            }
            new.push(d.to_path_buf());
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
        real.extend(new);
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

    /// Move whatever is at `abs` out of the way: into the backup on home,
    /// else gone.
    pub(super) fn displace(&self, abs: &Path, rel: &str) -> Result<()> {
        match &self.backup {
            Some(b) => b.keep(abs, &self.backup_rel(rel)),
            None => remove_tree(abs),
        }
    }

    /// Remove what a dead transfer left: temporary names only.
    pub fn clean(&self, temps: &[String]) -> Result<()> {
        for t in temps {
            let name = t.rsplit('/').next().unwrap_or(t);
            if !is_temp(name) {
                return Err(bad(t, "is not a temporary name; nothing removed"));
            }
            let abs = self.checked(t, false, false)?;
            match fs::remove_file(&abs) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
        }
        Ok(())
    }

    /// Remove `rel`, whose kind the source changes, before the new kind is
    /// written: for a directory, `under` (its contents the source lacks,
    /// children first) go first, and the directory only if that empties it.
    pub fn replace(&self, rel: &str, kind: EntryKind, under: &[(String, bool)]) -> Result<()> {
        let abs = self.path(rel, kind == EntryKind::Dir)?;
        if kind == EntryKind::Dir {
            self.delete(under)?;
            return match fs::remove_dir(&abs) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(_) => Err(bad(
                    rel,
                    "is a directory still holding what this unit does not sync (an \
                     excluded or git-ignored path); it is not replaced",
                )),
            };
        }
        match fs::symlink_metadata(&abs) {
            Ok(m) if m.is_dir() => Err(bad(rel, "is a directory now; it is not replaced")),
            Ok(_) => self.displace(&abs, rel),
            Err(_) => Ok(()),
        }
    }

    /// Make sure directory `e` exists. Its mode is set by [`Self::dir_modes`].
    pub fn mkdir(&self, e: &Entry) -> Result<()> {
        let abs = self.path(&e.rel, true)?;
        match fs::symlink_metadata(&abs) {
            Ok(m) if m.is_dir() => Ok(()),
            Ok(_) => Err(bad(&e.rel, "is not a directory, and was not replaced")),
            Err(_) => {
                fs::create_dir(&abs)?;
                fs::set_permissions(&abs, fs::Permissions::from_mode((e.mode & 0o7777) | 0o700))?;
                self.touch(&abs);
                Ok(())
            }
        }
    }

    /// Final directory modes, deepest first.
    pub fn dir_modes(&self, dirs: &[Entry]) -> Result<()> {
        let mut dirs: Vec<&Entry> = dirs.iter().collect();
        dirs.sort_by(|a, b| b.rel.cmp(&a.rel));
        for e in dirs {
            let abs = self.path(&e.rel, true)?;
            fs::set_permissions(&abs, fs::Permissions::from_mode(e.mode & 0o7777))?;
        }
        Ok(())
    }

    /// Delete each path (children first), the old copy displaced into the
    /// backup on home. A missing path is already gone; a directory still
    /// holding something (an excluded or ignored path, never deleted) stays,
    /// and is returned.
    pub fn delete(&self, list: &[(String, bool)]) -> Result<Vec<String>> {
        let mut kept = Vec::new();
        for (rel, is_dir) in list {
            let abs = match self.path(rel, *is_dir) {
                Ok(a) => a,
                // Under a path a replacement or this list removed already.
                Err(_) if self.root.join(rel).symlink_metadata().is_err() && !rel.is_empty() => {
                    continue;
                }
                Err(e) => return Err(e),
            };
            match fs::symlink_metadata(&abs) {
                Err(_) => {}
                Ok(m) if m.is_dir() => match fs::remove_dir(&abs) {
                    Ok(()) => self.touch(&abs),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(_) => kept.push(rel.clone()),
                },
                Ok(_) => {
                    self.displace(&abs, rel)?;
                    self.touch(&abs);
                }
            }
        }
        Ok(kept)
    }

    /// fsync every directory a rename or removal changed, so the names are
    /// as durable as the bytes.
    pub fn sync_dirs(&self) -> Result<()> {
        let dirs: Vec<PathBuf> = {
            let t = self.touched.lock().unwrap_or_else(|e| e.into_inner());
            t.iter().cloned().collect()
        };
        for d in dirs {
            if let Ok(f) = fs::File::open(&d) {
                let _ = f.sync_all();
            }
        }
        Ok(())
    }
}

#[path = "receive_file.rs"]
mod file;
pub use file::Incoming;

#[cfg(test)]
#[path = "receive_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "receive_backup_tests.rs"]
mod backup_tests;
