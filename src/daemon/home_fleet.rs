//! Home's parsed fleet.toml, and when it is read again.
//!
//! The file is the record of truth, and Brandon edits it by hand (narrowing a
//! cap, adding a root). So every look at the fleet first stats the file and
//! reloads it if its mtime, size or inode differ from what was loaded (v0.1
//! review, F11): a hand edit is enforced on the next request, without a
//! restart. The stat is taken *before* the read, so an edit that lands
//! between the two is seen on the following look rather than lost.
//!
//! A reload that fails (a typo, a half-saved file) is an error for that
//! request and every one after it until the file parses again. The old fleet
//! is never served in its place: a cap narrowed with a typo must not quietly
//! fall back to the wider one.

use crate::Result;
use crate::config::Fleet;
use std::ops::Deref;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::SystemTime;

/// What says the file changed. `None` when it cannot be stat'ed, which never
/// equals anything, so the next look tries (and fails) to read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    modified: SystemTime,
    len: u64,
    dev: u64,
    ino: u64,
}

fn stamp(path: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(path).ok()?;
    Some(Stamp {
        modified: m.modified().ok()?,
        len: m.len(),
        dev: m.dev(),
        ino: m.ino(),
    })
}

struct Cached {
    fleet: Fleet,
    stamp: Option<Stamp>,
}

impl Cached {
    fn load(&mut self, path: &Path) -> Result<()> {
        let stamp = stamp(path);
        self.fleet = Fleet::load(path)?;
        self.stamp = stamp;
        Ok(())
    }
}

/// The one mutex every reader and writer of fleet.toml goes through.
pub struct FleetCache {
    path: PathBuf,
    inner: Mutex<Cached>,
}

/// The fleet as the file says it is now, held under the mutex.
pub struct FleetGuard<'a> {
    guard: MutexGuard<'a, Cached>,
    path: &'a Path,
}

impl FleetCache {
    pub fn open(path: PathBuf) -> Result<Self> {
        let stamp = stamp(&path);
        let fleet = Fleet::load(&path)?;
        Ok(Self {
            path,
            inner: Mutex::new(Cached { fleet, stamp }),
        })
    }

    /// Lock, and reload first if the file changed behind our back.
    pub fn get(&self) -> Result<FleetGuard<'_>> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let now = stamp(&self.path);
        if now.is_none() || now != guard.stamp {
            guard.load(&self.path)?;
            eprintln!(
                "replicant: {} changed on disk; reloaded",
                self.path.display()
            );
        }
        Ok(FleetGuard {
            guard,
            path: &self.path,
        })
    }
}

impl FleetGuard<'_> {
    /// Reload after home's own write, under the same lock that wrote.
    pub fn reload(&mut self) -> Result<()> {
        self.guard.load(self.path)
    }
}

impl Deref for FleetGuard<'_> {
    type Target = Fleet;
    fn deref(&self) -> &Fleet {
        &self.guard.fleet
    }
}
