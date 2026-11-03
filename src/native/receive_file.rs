//! A file or link arriving: written under a temporary name in its own
//! directory, finished (mode, mtime, fsync), then renamed over its target.

use super::{Receiver, bad};
use crate::Result;
use crate::native::TEMP_PREFIX;
use crate::native::manifest::{Entry, EntryKind};
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Longest file name most filesystems take.
const NAME_MAX: usize = 255;

impl<'a> Receiver<'a> {
    /// The temporary name for `abs`, in its directory.
    fn temp_for(abs: &Path) -> PathBuf {
        let name = abs
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default();
        let temp = match TEMP_PREFIX.len() + name.len() <= NAME_MAX {
            true => format!("{TEMP_PREFIX}{name}"),
            false => {
                use sha2::Digest;
                let h = sha2::Sha256::digest(name.as_bytes());
                let hex: String = h[..12].iter().map(|b| format!("{b:02x}")).collect();
                format!("{TEMP_PREFIX}{hex}")
            }
        };
        abs.with_file_name(temp)
    }

    /// Start receiving file `e`: its bytes go to [`Incoming::write`].
    pub fn begin(&self, e: &Entry) -> Result<Incoming<'_, 'a>> {
        if e.kind != EntryKind::File {
            return Err(bad(&e.rel, "is not a file"));
        }
        let target = self.path(&e.rel, false)?;
        let temp = Self::temp_for(&target);
        let open = || {
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)
        };
        let file = match open() {
            Err(why) if why.kind() == io::ErrorKind::AlreadyExists => {
                fs::remove_file(&temp).map_err(|why| bad(&e.rel, why))?;
                open()
            }
            r => r,
        }
        .map_err(|why| bad(&e.rel, why))?;
        Ok(Incoming {
            rx: self,
            entry: e.clone(),
            target,
            temp,
            file: Some(file),
            written: 0,
        })
    }

    /// A whole small file at once.
    pub fn file(&self, e: &Entry, bytes: &[u8]) -> Result<()> {
        let mut inc = self.begin(e)?;
        inc.write(bytes)?;
        inc.finish()
    }

    /// Make link `e`, replacing whatever non-directory is there.
    pub fn symlink(&self, e: &Entry) -> Result<()> {
        let target = self.path(&e.rel, false)?;
        let link = e
            .link
            .as_deref()
            .ok_or_else(|| bad(&e.rel, "a link with no target"))?;
        let temp = Self::temp_for(&target);
        let _ = fs::remove_file(&temp);
        std::os::unix::fs::symlink(link, &temp).map_err(|why| bad(&e.rel, why))?;
        self.commit(&temp, &target, &e.rel)
    }

    /// Rename a finished temporary over its target, the old one displaced
    /// first on home (into the backup).
    fn commit(&self, temp: &Path, target: &Path, rel: &str) -> Result<()> {
        let r = self
            .guard(target, rel)
            .and_then(|()| match fs::symlink_metadata(target) {
                Ok(m) if m.is_dir() => Err(bad(rel, "is a directory, and was not replaced")),
                Ok(m) if crate::repos::is_special(&m) => Err(bad(
                    rel,
                    "is a socket, FIFO or device on this side, and was not replaced",
                )),
                Ok(_) if self.backup.is_some() => self.displace(target, rel),
                _ => Ok(()),
            })
            .and_then(|()| fs::rename(temp, target).map_err(|why| bad(rel, why)));
        if r.is_err() {
            let _ = fs::remove_file(temp);
        }
        self.touch(target);
        r
    }
}

/// A file being received.
pub struct Incoming<'r, 'a> {
    rx: &'r Receiver<'a>,
    entry: Entry,
    target: PathBuf,
    temp: PathBuf,
    file: Option<fs::File>,
    written: u64,
}

impl Incoming<'_, '_> {
    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.written += bytes.len() as u64;
        if self.written > self.entry.size {
            return Err(bad(&self.entry.rel, "more bytes arrived than it has"));
        }
        let rel = &self.entry.rel;
        let f = self.file.as_mut().expect("open until finished");
        f.write_all(bytes).map_err(|why| bad(rel, why))
    }

    /// Mode, mtime, fsync, then rename into place.
    pub fn finish(mut self) -> Result<()> {
        let e = &self.entry;
        if self.written != e.size {
            return Err(bad(
                &e.rel,
                format!("{} of its {} bytes arrived", self.written, e.size),
            ));
        }
        let f = self.file.take().expect("open until finished");
        let when = mtime(e.mtime_ns);
        let finished = f
            .set_permissions(fs::Permissions::from_mode(e.mode & 0o7777))
            .and_then(|()| f.set_times(fs::FileTimes::new().set_modified(when)))
            .and_then(|()| crate::native::fsync(&f));
        finished.map_err(|why| bad(&e.rel, why))?;
        drop(f);
        let (temp, target) = (self.temp.clone(), self.target.clone());
        self.rx.commit(&temp, &target, &e.rel)
    }
}

impl Drop for Incoming<'_, '_> {
    /// Unfinished: the temporary goes.
    fn drop(&mut self) {
        if self.file.take().is_some() {
            let _ = fs::remove_file(&self.temp);
        }
    }
}

/// Nanoseconds since the epoch as a time (before it, too).
fn mtime(ns: i64) -> SystemTime {
    let d = Duration::from_nanos(ns.unsigned_abs());
    match ns >= 0 {
        true => SystemTime::UNIX_EPOCH + d,
        false => SystemTime::UNIX_EPOCH - d,
    }
}
