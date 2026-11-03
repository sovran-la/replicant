//! Home is also the backup (PLAN.md): before a push replaces or deletes a
//! path on home, the receiver *renames* what is there into the push run's
//! backup directory, `backup/<host>/<push>/run-<n>/<rel>`. A rename on the
//! same filesystem copies nothing and cannot half-happen. When the backup
//! directory is on another filesystem (`EXDEV`), the path is copied there
//! with its mode, mtime and link target, under a temporary name and renamed
//! once whole, so what is in the backup is always all of it; and then removed.
//!
//! A path about to be replaced by a rename stays where it is until then: the
//! backup gets a hard link to it ([`Backup::keep_beside`]), so the target is
//! never absent, not after a crash and not after a failed rename.
//!
//! A path already kept in this run is not kept again: the run's copy is
//! the one from before the run touched it, which is what rollback needs.
//!
//! Directories are not kept, since nothing in one is lost by removing it once
//! it is empty; but what they were is what a rollback must put back. Each one
//! a run removes, creates or changes the mode of is noted first, one line in
//! `run-<n>.dirs` beside the run's directory ([`Backup::note_dir`]): a record
//! that, like the directory, survives a leaf dying before it reports.

use super::temp_name;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io::{self, BufRead as _, Write as _};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// How a path is moved into the backup: `std::fs::rename`, or a stand-in
/// that fails the way another filesystem would.
pub type Rename = fn(&Path, &Path) -> io::Result<()>;

fn rename(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

fn hard_link(from: &Path, to: &Path) -> io::Result<()> {
    fs::hard_link(from, to)
}

/// Copy `from` to `to`, whole or not at all: under a temporary name there, and
/// renamed once it is finished. `Backup::kept`'s reader skips temporary names.
fn copy_whole(from: &Path, to: &Path) -> io::Result<()> {
    let name = to.file_name().map(|n| n.to_string_lossy());
    let temp = to.with_file_name(temp_name(&name.unwrap_or_default()));
    let _ = remove_tree(&temp);
    let copied = copy_tree(from, &temp).and_then(|()| fs::rename(&temp, to));
    if copied.is_err() {
        let _ = remove_tree(&temp);
    }
    copied
}

/// What a directory was before a run first touched it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirWas {
    #[serde(rename = "p")]
    pub rel: String,
    /// Its permission bits; `None`: it did not exist, the run made it.
    #[serde(rename = "m")]
    pub mode: Option<u32>,
}

/// What the run at `run_dir` noted, each directory's first line only: that is
/// what it was before the run. A line a dying process tore is not one.
pub fn dirs_noted(run_dir: &Path) -> Result<Vec<DirWas>> {
    let file = match fs::File::open(run_dir.with_extension("dirs")) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.into()),
    };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for line in io::BufReader::new(file).lines() {
        if let Ok(d) = serde_json::from_str::<DirWas>(&line?)
            && seen.insert(d.rel.clone())
        {
            out.push(d);
        }
    }
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct Backup {
    dir: PathBuf,
    rename: Rename,
    link: Rename,
}

impl Backup {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            rename,
            link: hard_link,
        }
    }

    /// With `rename` standing in for both the rename and the hard link.
    pub fn with_rename(dir: PathBuf, rename: Rename) -> Self {
        Self {
            dir,
            rename,
            link: rename,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Keep `abs` (the unit-relative `rel`, a file or link) in the backup and
    /// leave it where it is, for a rename to replace.
    pub fn keep_beside(&self, abs: &Path, rel: &str) -> Result<()> {
        self.real_parents(rel)?;
        let dest = self.dir.join(rel);
        if fs::symlink_metadata(&dest).is_ok() {
            // Kept earlier in this run: that copy is the original.
            return Ok(());
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        // A link's own name, not a second name for what it points at: rebuilt.
        let linked = fs::symlink_metadata(abs)?.is_file() && (self.link)(abs, &dest).is_ok();
        if !linked {
            copy_whole(abs, &dest).map_err(|e| {
                Error::Transfer(format!("could not back up {rel} before replacing it: {e}"))
            })?;
        }
        Ok(())
    }

    /// Note what directory `rel` was, before it is removed, made or changed.
    /// A failure here stops the change: rollback could not undo it.
    pub fn note_dir(&self, rel: &str, mode: Option<u32>) -> Result<()> {
        let mut line = serde_json::to_vec(&DirWas {
            rel: rel.to_string(),
            mode,
        })
        .map_err(|e| Error::Transfer(format!("encode: {e}")))?;
        line.push(b'\n');
        let note = || -> io::Result<()> {
            if let Some(parent) = self.dir.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(self.dir.with_extension("dirs"))?
                .write_all(&line)
        };
        note().map_err(|e| Error::Transfer(format!("could not note what {rel} was: {e}")))
    }

    /// Nothing between the run's directory and `rel` is a file or link: the
    /// run keeps links as links, and one must never be followed out of here.
    fn real_parents(&self, rel: &str) -> Result<()> {
        let mut d = self.dir.clone();
        let comps: Vec<&str> = rel.split('/').collect();
        for c in &comps[..comps.len() - 1] {
            d.push(c);
            if fs::symlink_metadata(&d).is_ok_and(|m| !m.is_dir()) {
                return Err(Error::Transfer(format!(
                    "could not back up {rel}: {c} is already kept in this run as something that \
                     is not a directory"
                )));
            }
        }
        Ok(())
    }

    /// Move `abs` (the unit-relative `rel`, a file, link or directory) out of
    /// the way into the backup.
    pub fn keep(&self, abs: &Path, rel: &str) -> Result<()> {
        self.real_parents(rel)?;
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
                copy_whole(abs, &dest).map_err(|e| {
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

/// Whether a path in a run's directory is a temporary name, or under one: a
/// copy that did not finish, which is not something the run kept.
pub fn is_unfinished(rel: &str) -> bool {
    rel.split('/').any(super::is_temp)
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
    } else if !ft.is_file() {
        // Copying a FIFO would block for a writer that never comes.
        return Err(io::Error::other(format!(
            "{} is a socket, FIFO or device",
            from.display()
        )));
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
