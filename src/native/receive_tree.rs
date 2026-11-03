//! The receiver's work on the tree itself: what a dead transfer left, kind
//! changes, directories and their modes, deletions, and making the names
//! durable.

use super::{Receiver, bad};
use crate::Result;
use crate::native::is_temp;
use crate::native::manifest::{Entry, EntryKind};
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

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
            self.guard(&abs, rel)?;
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
            Ok(_) => {
                self.guard(&abs, rel)?;
                self.displace(&abs, rel)
            }
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
            self.guard(&abs, rel)?;
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
                let _ = crate::native::fsync(&f);
            }
        }
        Ok(())
    }
}
