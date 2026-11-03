//! The sending side's reads: the leaf on a push, home on a pull.
//!
//! What is sent is what is there when it is sent, with the metadata it has
//! then (as rsync did), never followed through a link: an entry
//! that is no longer a file or link of that name is skipped, and the
//! transfer's verification then finds the difference. A file that changes
//! while it is read is not sent at all ([`Read::Changed`]).

use super::manifest::{Entry, EntryKind, mtime_ns};
use crate::exclude::Excludes;
use crate::{Error, Result, paths};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

pub struct Source<'a> {
    root: PathBuf,
    is_file: bool,
    policy: Option<&'a Excludes>,
}

/// One small entry, read.
pub enum Read {
    /// A file's entry as it is now, and its bytes; or a link's entry.
    Whole(Entry, Vec<u8>),
    /// Gone, a directory now, or changed while it was read: not sent.
    Changed,
}

impl<'a> Source<'a> {
    /// The unit at `root`: a directory unit's symlinked root is followed.
    pub fn open(root: &Path, is_file: bool, policy: Option<&'a Excludes>) -> Result<Self> {
        let root = match fs::symlink_metadata(root) {
            Ok(m) if m.file_type().is_symlink() && !is_file => fs::canonicalize(root)?,
            _ => root.to_path_buf(),
        };
        Ok(Self {
            root,
            is_file,
            policy,
        })
    }

    /// The absolute path of `rel`, once it is known to be inside the unit,
    /// allowed by policy, and under real directories only.
    pub fn path(&self, rel: &str) -> Result<PathBuf> {
        let refuse = |why: &str| Error::Transfer(format!("{rel:?}: {why}"));
        if rel.is_empty() {
            return match self.is_file {
                true => Ok(self.root.clone()),
                false => Err(refuse("is the unit's own directory")),
            };
        }
        if self.is_file {
            return Err(refuse("is inside a unit that is a single file"));
        }
        paths::check_rel(rel).map_err(refuse)?;
        if self.policy.is_some_and(|p| p.path_excluded(rel, false)) {
            return Err(refuse("is excluded on home, which does not send it"));
        }
        let mut d = self.root.clone();
        let comps: Vec<&str> = rel.split('/').collect();
        for c in &comps[..comps.len() - 1] {
            d.push(c);
            if !fs::symlink_metadata(&d).is_ok_and(|m| m.is_dir()) {
                return Err(refuse("lies under something that is not a directory"));
            }
        }
        Ok(self.root.join(rel))
    }

    /// The entry `rel` names as it is now, if it is still a file or link.
    pub fn now(&self, rel: &str) -> Result<Option<(PathBuf, Entry)>> {
        let abs = self.path(rel)?;
        match fs::symlink_metadata(&abs) {
            Ok(m) if !m.is_dir() => Ok(Some((abs.clone(), Entry::of(&abs, rel, &m)?))),
            _ => Ok(None),
        }
    }

    /// A small file's bytes (or a link), whole.
    pub fn read(&self, rel: &str) -> Result<Read> {
        let Some((abs, e)) = self.now(rel)? else {
            return Ok(Read::Changed);
        };
        if e.kind == EntryKind::Symlink {
            return Ok(Read::Whole(e, vec![]));
        }
        let mut f = match open(&abs) {
            Some(f) => f,
            None => return Ok(Read::Changed),
        };
        let mut bytes = Vec::with_capacity(e.size as usize);
        f.read_to_end(&mut bytes)?;
        match unchanged(&f, &e) && bytes.len() as u64 == e.size {
            true => Ok(Read::Whole(e, bytes)),
            false => Ok(Read::Changed),
        }
    }
}

/// Open a file for reading without following a link swapped in since it was
/// looked at; `None` if that is what happened, or it is gone.
pub fn open(abs: &Path) -> Option<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(abs)
        .ok()
}

/// Whether the open file still has the size and mtime `e` recorded.
pub fn unchanged(f: &fs::File, e: &Entry) -> bool {
    f.metadata()
        .is_ok_and(|m| m.is_file() && m.len() == e.size && mtime_ns(&m) == e.mtime_ns)
}

#[cfg(test)]
#[path = "source_tests.rs"]
mod tests;
