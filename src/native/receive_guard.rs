//! The last look before a pull overwrites or deletes a path: the diff was
//! taken moments ago, and a leaf has no backup, so a path that moved since
//! fails the unit (its marker stays, and the next sync's plan shows the
//! change) rather than losing what was written there.

use super::bad;
use crate::Result;
use crate::native::manifest::{Entry, Manifest};
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// Whether `now` is what `then` recorded. Mode is left out: a chmod loses no
/// work, and a mode-only change is resent whole anyway.
fn same(then: &Entry, now: &Entry) -> bool {
    then.kind == now.kind
        && then.size == now.size
        && then.mtime_ns == now.mtime_ns
        && then.link == now.link
}

/// `abs` (`rel`) is what `target` lists, or is gone. A path `target` does not
/// list must not exist either, unless it is an entry `target` knows under
/// another spelling (a case-insensitive filesystem: home's `foo.txt` lands
/// on the leaf's `FOO.txt`, the same file).
pub(super) fn still(target: &Manifest, abs: &Path, rel: &str) -> Result<()> {
    let meta = match fs::symlink_metadata(abs) {
        Ok(m) => m,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(());
        }
        Err(e) => return Err(bad(rel, e)),
    };
    let moved = |what: &str| {
        bad(
            rel,
            format!(
                "{what} on this side since the plan was made; it was left alone. Run \
                 `replicant sync` again"
            ),
        )
    };
    let now = Entry::of(abs, rel, &meta)?;
    match target
        .get(rel)
        .or_else(|| spelled_otherwise(target, abs, rel, &meta))
    {
        Some(then) if same(then, &now) => Ok(()),
        Some(_) => Err(moved("changed")),
        None => Err(moved("appeared")),
    }
}

/// The entry `target` holds for what `abs` really is: the name on disk of the
/// same inode in its directory. Only asked of a path `target` does not list,
/// which is rare, so listing a directory here costs nothing that matters.
fn spelled_otherwise<'t>(
    target: &'t Manifest,
    abs: &Path,
    rel: &str,
    meta: &fs::Metadata,
) -> Option<&'t Entry> {
    let same_file = |e: &fs::DirEntry| {
        e.path().file_name() != abs.file_name()
            && fs::symlink_metadata(e.path()).is_ok_and(|m| m.ino() == meta.ino())
    };
    let name = fs::read_dir(abs.parent()?)
        .ok()?
        .flatten()
        .find(same_file)?
        .file_name();
    let name = name.to_str()?;
    match rel.rsplit_once('/') {
        Some((dir, _)) => target.get(&format!("{dir}/{name}")),
        None => target.get(name),
    }
}
