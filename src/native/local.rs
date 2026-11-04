//! Home's own list operations on a unit, through the receiver (so every
//! write of a synced path stays in one place, under its checks): copying
//! listed files with their metadata (a rollback restoring from a push's
//! backup, keeping what the push wrote), deleting exactly a list (a rollback
//! removing what a push created), and putting directories back as they were
//! ([`open_dirs`] before the files go in, [`set_dir_modes`] after).

use super::manifest::{Entry, EntryKind};
use super::receive::Receiver;
use super::source;
use crate::changes::{Change, ChangeKind, Itemized};
use crate::{Error, Result, paths};
use std::fs;
use std::io::Read as _;
use std::path::Path;

fn dir_entry(rel: &str, mode: u32) -> Entry {
    Entry {
        rel: rel.to_string(),
        kind: EntryKind::Dir,
        size: 0,
        mtime_ns: 0,
        mode,
        link: None,
    }
}

/// Make each of `dirs` (`(path, mode)`) exist and be fillable, parents first.
/// None: nothing is touched, `root` included (a receiver makes a missing one,
/// and a single-file unit's root is no directory).
pub fn open_dirs(root: &Path, dirs: &[(String, u32)]) -> Result<()> {
    if dirs.is_empty() {
        return Ok(());
    }
    let rx = Receiver::open(root, false, None, None)?;
    let mut sorted: Vec<&(String, u32)> = dirs.iter().collect();
    sorted.sort();
    for (rel, mode) in sorted {
        rx.mkdir(&dir_entry(rel, *mode))?;
    }
    Ok(())
}

/// Give each of `dirs` its final mode, deepest first.
pub fn set_dir_modes(root: &Path, dirs: &[(String, u32)]) -> Result<()> {
    if dirs.is_empty() {
        return Ok(());
    }
    let rx = Receiver::open(root, false, None, None)?;
    let list: Vec<Entry> = dirs.iter().map(|(r, m)| dir_entry(r, *m)).collect();
    rx.dir_modes(&list)?;
    rx.sync_dirs()
}

/// Whether `rel` under `root` is a file or link, reached through real
/// directories only: what a rollback can keep a copy of.
pub fn holds_file(root: &Path, rel: &str) -> bool {
    under_real_dirs(root, rel) && fs::symlink_metadata(root.join(rel)).is_ok_and(|m| !m.is_dir())
}

/// Whether every directory between `root` and `rel` is a real directory.
fn under_real_dirs(root: &Path, rel: &str) -> bool {
    let comps: Vec<&str> = rel.split('/').collect();
    let mut d = root.to_path_buf();
    comps[..comps.len().saturating_sub(1)].iter().all(|c| {
        d.push(c);
        fs::symlink_metadata(&d).is_ok_and(|m| m.is_dir())
    })
}

/// Copy each listed file or link (relative to `from`) to the same path
/// under `to`, with its mode and mtime, creating missing directories (with
/// their modes under `from`; existing ones are left as they are). Never
/// through a link on either side.
pub fn copy_listed(from: &Path, to: &Path, files: &[String]) -> Result<Itemized> {
    fs::create_dir_all(to)?;
    let rx = Receiver::open(to, false, None, None)?;
    let mut out = Itemized::default();
    let mut dirs: Vec<Entry> = Vec::new();
    for rel in files {
        paths::check_rel(rel).map_err(|why| Error::Path(format!("{rel:?}: {why}")))?;
        if !under_real_dirs(from, rel) {
            return Err(Error::Transfer(format!(
                "{rel:?}: lies under a link in {}",
                from.display()
            )));
        }
        let comps: Vec<&str> = rel.split('/').collect();
        for i in 1..comps.len() {
            let d = comps[..i].join("/");
            // Only a directory made here takes `from`'s mode: one already
            // there keeps its own (a backup's directories are made with a
            // default mode, and say nothing about the originals).
            if fs::symlink_metadata(to.join(&d)).is_ok() {
                continue;
            }
            let m = fs::symlink_metadata(from.join(&d))?;
            let e = Entry::of(&from.join(&d), &d, &m)?;
            rx.mkdir(&e)?;
            dirs.push(e);
        }
        let abs = from.join(rel);
        let e = Entry::of(&abs, rel, &fs::symlink_metadata(&abs)?)?;
        match e.kind {
            EntryKind::Symlink => rx.symlink(&e)?,
            EntryKind::Dir => {
                return Err(Error::Transfer(format!(
                    "{rel:?} is a directory, not a file"
                )));
            }
            EntryKind::File => {
                let mut f = source::open(&abs)
                    .ok_or_else(|| Error::Transfer(format!("{rel:?} could not be read")))?;
                let mut inc = rx.begin(&e)?;
                let mut buf = vec![0u8; super::CHUNK];
                loop {
                    let n = f.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    inc.write(&buf[..n])?;
                }
                inc.finish()?;
            }
        }
        out.changes.push(Change {
            kind: ChangeKind::Create,
            path: rel.clone(),
            is_dir: false,
        });
    }
    rx.dir_modes(&dirs)?;
    rx.sync_dirs()?;
    Ok(out)
}

/// Delete exactly `list` (`(path, is_dir)`, relative to `root`) and nothing
/// else: a listed directory goes only once what is listed in it has and it
/// is empty; a path missing, or under a link, is left alone.
pub fn delete_listed(root: &Path, list: &[(String, bool)]) -> Result<Itemized> {
    let rx = Receiver::open(root, false, None, None)?;
    let mut list: Vec<(String, bool)> = list
        .iter()
        .filter(|(p, _)| paths::check_rel(p).is_ok() && under_real_dirs(root, p))
        .filter(|(p, _)| fs::symlink_metadata(root.join(p)).is_ok())
        .cloned()
        .collect();
    list.sort_by(|a, b| b.0.cmp(&a.0));
    let kept = rx.delete(&list)?;
    let changes = list
        .into_iter()
        .filter(|(p, _)| !kept.contains(p))
        .map(|(path, is_dir)| Change {
            kind: ChangeKind::Delete,
            path,
            is_dir,
        })
        .collect();
    rx.sync_dirs()?;
    Ok(Itemized {
        changes,
        notes: vec![],
    })
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
