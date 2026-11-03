//! The receiver's work on the tree itself: what a dead transfer left, kind
//! changes, directories and their modes, deletions, and making the names
//! durable.

use super::{Receiver, bad};
use crate::native::is_temp;
use crate::native::manifest::{Entry, EntryKind};
use crate::{Error, Result};
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

fn mode_of(m: &fs::Metadata) -> u32 {
    m.permissions().mode() & 0o7777
}

impl Receiver<'_> {
    /// Remove what a dead transfer left: temporary names only.
    pub fn clean(&self, temps: &[String]) -> Result<()> {
        for t in temps {
            let name = t.rsplit('/').next().unwrap_or(t);
            if !is_temp(name) {
                return Err(bad(t, "is not a temporary name; nothing removed"));
            }
            let abs = self.checked(t, false, false)?;
            match fs::remove_file(&abs) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(bad(t, e)),
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
            self.guard(&abs, rel)?;
            self.delete(under)?;
            if let Ok(m) = fs::symlink_metadata(&abs) {
                self.note_dir(rel, Some(mode_of(&m)))?;
            }
            return match fs::remove_dir(&abs) {
                Ok(()) => {
                    self.touch(&abs);
                    Ok(())
                }
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
            Ok(_) => {
                self.guard(&abs, rel)?;
                self.touch(&abs);
                self.displace(&abs, rel)
            }
            Err(_) => Ok(()),
        }
    }

    /// Make sure directory `e` exists and can be filled: a new one, or one
    /// already there, is owner-writable until [`Self::dir_modes`] sets its
    /// final mode.
    pub fn mkdir(&self, e: &Entry) -> Result<()> {
        let abs = self.path(&e.rel, true)?;
        let writable = fs::Permissions::from_mode((e.mode & 0o7777) | 0o700);
        match fs::symlink_metadata(&abs) {
            Ok(m) if m.is_dir() => {
                if mode_of(&m) != writable.mode() {
                    self.note_dir(&e.rel, Some(mode_of(&m)))?;
                    fs::set_permissions(&abs, writable).map_err(|why| bad(&e.rel, why))?;
                }
                Ok(())
            }
            Ok(_) => Err(bad(&e.rel, "is not a directory, and was not replaced")),
            Err(_) => {
                self.note_dir(&e.rel, None)?;
                fs::create_dir(&abs).map_err(|why| bad(&e.rel, why))?;
                fs::set_permissions(&abs, writable).map_err(|why| bad(&e.rel, why))?;
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
            // set_permissions follows a link: only a real directory gets here.
            let m = fs::symlink_metadata(&abs).map_err(|why| bad(&e.rel, why))?;
            if !m.is_dir() {
                return Err(bad(&e.rel, "is not a directory; no mode was set"));
            }
            if mode_of(&m) != e.mode & 0o7777 {
                self.note_dir(&e.rel, Some(mode_of(&m)))?;
            }
            fs::set_permissions(&abs, fs::Permissions::from_mode(e.mode & 0o7777))
                .map_err(|why| bad(&e.rel, why))?;
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
            self.guard(&abs, rel)?;
            match fs::symlink_metadata(&abs) {
                Err(_) => {}
                Ok(m) if m.is_dir() => match self
                    .note_dir(rel, Some(mode_of(&m)))
                    .and_then(|()| Ok(fs::remove_dir(&abs)?))
                {
                    Ok(()) => self.touch(&abs),
                    Err(Error::Io(e)) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(Error::Io(_)) => kept.push(rel.clone()),
                    Err(e) => return Err(e),
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
    /// as durable as the bytes. A failure is the transfer's: no baseline may
    /// say both sides agree on names that may not be there after a power cut.
    pub fn sync_dirs(&self) -> Result<()> {
        let dirs: Vec<PathBuf> = {
            let t = self.touched.lock().unwrap_or_else(|e| e.into_inner());
            t.iter().cloned().collect()
        };
        sync_each(&dirs, crate::native::fsync)
    }
}

/// `sync` each of `dirs`. One removed since (or that a file took the place of,
/// or of its parent) was renamed out of or deleted: its parent holds that name. One this user may not open was given
/// that mode by the source, and its names cannot be flushed from here. A
/// filesystem that does not fsync directories says so with `EINVAL`.
pub(super) fn sync_each(dirs: &[PathBuf], sync: fn(&fs::File) -> io::Result<()>) -> Result<()> {
    for d in dirs {
        let f = match fs::File::open(d) {
            Ok(f) => f,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound
                        | io::ErrorKind::NotADirectory
                        | io::ErrorKind::PermissionDenied
                ) =>
            {
                continue;
            }
            Err(e) => return Err(Error::Transfer(format!("{}: {e}", d.display()))),
        };
        match sync(&f) {
            Err(e) if e.raw_os_error() != Some(libc::EINVAL) => {
                return Err(Error::Transfer(format!(
                    "{} could not be synced: {e}",
                    d.display()
                )));
            }
            _ => {}
        }
    }
    Ok(())
}
