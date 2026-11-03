//! The change list: what makes a target's manifest equal its source's.
//!
//! An entry is sent when the target lacks it or holds it with a different
//! kind, size, mtime, mode or link target. A symlink compares by its
//! target only: its mode is not portable (macOS keeps one, Linux has
//! 0777) and no receiver can set it. A directory has no size or mtime
//! worth comparing, so only its mode can change it. Whatever the target
//! holds that the source does not is deleted.
//!
//! The order is the order a receiver needs: kind changes first (a file
//! must go before a directory can take its name), then directories,
//! parents before children, then files and links, then deletions,
//! children before parents.

use super::manifest::{Entry, EntryKind, Manifest};
use crate::changes::{Change, ChangeKind, Itemized};
use std::collections::HashMap;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeList {
    /// Target paths whose kind the source changes, with the target's kind:
    /// removed before anything is written there.
    pub replace: Vec<(String, EntryKind)>,
    /// Directories to create, or whose mode changes (the source's entries),
    /// parents first.
    pub mkdir: Vec<Entry>,
    /// Files and links to send (the source's entries), in path order.
    pub send: Vec<Entry>,
    /// Target paths the source does not have, `(path, is_dir)`, children
    /// first.
    pub delete: Vec<(String, bool)>,
}

impl ChangeList {
    pub fn is_empty(&self) -> bool {
        self.replace.is_empty()
            && self.mkdir.is_empty()
            && self.send.is_empty()
            && self.delete.is_empty()
    }

    /// Nothing is left to do but delete directories: what a target looks
    /// like after a transfer whose directories still hold what the unit
    /// excludes (a git-ignored file, a build directory), which no transfer
    /// deletes.
    pub fn only_kept_dirs(&self) -> bool {
        self.replace.is_empty()
            && self.mkdir.is_empty()
            && self.send.is_empty()
            && self.delete.iter().all(|(_, d)| *d)
    }

    /// Files and links to send, and their bytes.
    pub fn totals(&self) -> (u64, u64) {
        self.send
            .iter()
            .fold((0, 0), |(n, b), e| (n + 1, b + e.size))
    }
}

/// Whether `t` already is what `s` says, so nothing need be sent.
fn same(s: &Entry, t: &Entry) -> bool {
    match s.kind {
        EntryKind::File => s.size == t.size && s.mtime_ns == t.mtime_ns && s.mode == t.mode,
        EntryKind::Symlink => s.link == t.link,
        EntryKind::Dir => s.mode == t.mode,
    }
}

/// The change list that makes `target` equal `source`.
pub fn diff(source: &Manifest, target: &Manifest) -> ChangeList {
    let have: HashMap<&str, &Entry> = target.entries.iter().map(|e| (e.rel.as_str(), e)).collect();
    let mut out = ChangeList::default();
    for s in &source.entries {
        let t = have.get(s.rel.as_str());
        match t {
            Some(t) if t.kind != s.kind => out.replace.push((s.rel.clone(), t.kind)),
            Some(t) if same(s, t) => continue,
            _ => {}
        }
        match s.kind {
            EntryKind::Dir => out.mkdir.push(s.clone()),
            _ => out.send.push(s.clone()),
        }
    }
    let want: HashMap<&str, ()> = source
        .entries
        .iter()
        .map(|e| (e.rel.as_str(), ()))
        .collect();
    out.delete = target
        .entries
        .iter()
        .filter(|t| !want.contains_key(t.rel.as_str()))
        .map(|t| (t.rel.clone(), t.is_dir()))
        .collect();
    out.replace.sort();
    out.mkdir.sort();
    out.send.sort();
    out.delete.sort_by(|a, b| b.0.cmp(&a.0));
    out
}

/// The change list as a dry run reports it, in path order: a kind change is
/// a deletion and a creation; a file whose mode alone differs is `Attr`.
pub fn itemize(changes: &ChangeList, target: &Manifest) -> Itemized {
    let replaced: HashMap<&str, ()> = changes
        .replace
        .iter()
        .map(|(p, _)| (p.as_str(), ()))
        .collect();
    let mut out: Vec<Change> = Vec::new();
    let change = |kind, e: &Entry| Change {
        kind,
        path: e.rel.clone(),
        is_dir: e.is_dir(),
    };
    for (p, kind) in &changes.replace {
        out.push(Change {
            kind: ChangeKind::Delete,
            path: p.clone(),
            is_dir: *kind == EntryKind::Dir,
        });
    }
    for e in changes.mkdir.iter().chain(&changes.send) {
        let kind = match target.get(&e.rel) {
            None => ChangeKind::Create,
            Some(_) if replaced.contains_key(e.rel.as_str()) => ChangeKind::Create,
            Some(t) if e.kind == EntryKind::Dir => {
                debug_assert_eq!(t.kind, EntryKind::Dir);
                ChangeKind::Attr
            }
            Some(t) if e.size == t.size && e.mtime_ns == t.mtime_ns && e.link == t.link => {
                ChangeKind::Attr
            }
            Some(_) => ChangeKind::Update,
        };
        out.push(change(kind, e));
    }
    for (p, is_dir) in &changes.delete {
        out.push(Change {
            kind: ChangeKind::Delete,
            path: p.clone(),
            is_dir: *is_dir,
        });
    }
    // Stable: a replaced path's deletion stays ahead of its creation.
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Itemized {
        changes: out,
        notes: vec![],
    }
}

#[cfg(test)]
#[path = "diff_tests.rs"]
mod tests;
