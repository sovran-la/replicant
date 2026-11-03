//! Home is also the backup (PLAN.md): before a push replaces or deletes a
//! path on home, the receiver *renames* what is there into the push run's
//! backup directory, `backup/<host>/<push>/run-<n>/<rel>`. A rename on the
//! same filesystem copies nothing and cannot half-happen. When the backup
//! directory is on another filesystem (`EXDEV`), the path is copied there
//! with its mode, mtime and link target, and then removed.
//!
//! A path already kept in this run is not kept again: the run's copy is
//! the one from before the run touched it, which is what rollback needs.

use crate::{Error, Result};
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// How a path is moved into the backup: `std::fs::rename`, or a stand-in
/// that fails the way another filesystem would.
pub type Rename = fn(&Path, &Path) -> io::Result<()>;

fn rename(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

#[derive(Debug, Clone)]
pub struct Backup {
    dir: PathBuf,
    rename: Rename,
}

impl Backup {
    pub fn new(dir: PathBuf) -> Self {
        Self::with_rename(dir, rename)
    }

    pub fn with_rename(dir: PathBuf, rename: Rename) -> Self {
        Self { dir, rename }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Move `abs` (the unit-relative `rel`, a file, link or directory) out of
    /// the way into the backup.
    pub fn keep(&self, abs: &Path, rel: &str) -> Result<()> {
        let dest = self.dir.join(rel);
        if fs::symlink_metadata(&dest).is_ok() {
            // Kept earlier in this run: that copy is the original.
            return remove_tree(abs);
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        match (self.rename)(abs, &dest) {
            Ok(()) => Ok(()),
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
                copy_tree(abs, &dest).map_err(|e| {
                    Error::Transfer(format!(
                        "could not back up {rel} across filesystems before replacing it: {e}"
                    ))
                })?;
                remove_tree(abs)
            }
            Err(e) => Err(Error::Transfer(format!(
                "could not back up {rel} before replacing it: {e}"
            ))),
        }
    }
}

/// Copy a file, link or directory tree with its modes and file mtimes.
pub fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(from)?, to)?;
    } else if ft.is_dir() {
        fs::create_dir(to)?;
        for e in fs::read_dir(from)? {
            let e = e?;
            copy_tree(&e.path(), &to.join(e.file_name()))?;
        }
        fs::set_permissions(to, fs::Permissions::from_mode(meta.permissions().mode()))?;
    } else {
        fs::copy(from, to)?;
        // Read-only is enough: the owner may set times on any mode.
        let f = fs::File::open(to)?;
        f.set_times(fs::FileTimes::new().set_modified(meta.modified()?))?;
        super::fsync(&f)?;
    }
    Ok(())
}

/// Remove a file, link or directory tree; a missing path is no error.
pub fn remove_tree(abs: &Path) -> Result<()> {
    match fs::symlink_metadata(abs) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
        Ok(m) if m.is_dir() => Ok(fs::remove_dir_all(abs)?),
        Ok(_) => Ok(fs::remove_file(abs)?),
    }
}

#[cfg(test)]
#[path = "backup_tests.rs"]
mod tests;
