//! What undoing a push takes, worked out from its record alone.

use super::super::backup::Push;
use std::collections::{BTreeMap, BTreeSet};

/// What undoing a push takes: per run, the files to restore from it; the
/// paths to delete; the files nothing was backed up for.
#[derive(Debug, Default, PartialEq, Eq)]
pub(in crate::daemon) struct Undo {
    pub restore: Vec<Vec<String>>,
    pub delete: Vec<(String, bool)>,
    pub unbacked: Vec<String>,
    /// Runs never reported on: what they created is not known.
    pub unreported: Vec<usize>,
    /// Directories to bring back, or put back the mode of: `(path, mode)`.
    pub dirs: Vec<(String, u32)>,
}

/// Each path decided by the first line that touched it, in run order. A kind
/// change is two lines on one path, the old kind's and then the new kind's:
/// the first says what comes back, and the second is what must go first.
pub(in crate::daemon) fn undo(push: &Push) -> Undo {
    let mut u = Undo {
        restore: vec![vec![]; push.runs.len()],
        ..Default::default()
    };
    let mut first: BTreeMap<String, bool> = BTreeMap::new();
    let mut delete: BTreeSet<(String, bool)> = BTreeSet::new();
    let mut noted = BTreeSet::new();
    for (i, run) in push.runs.iter().enumerate() {
        for d in &run.dirs {
            if crate::paths::check_rel(&d.rel).is_err() || !noted.insert(d.rel.clone()) {
                continue;
            }
            match d.mode {
                Some(mode) => u.dirs.push((d.rel.clone(), mode)),
                None => {
                    delete.insert((d.rel.clone(), true));
                }
            }
        }
        for c in &run.changes {
            let Some((kind, path)) = c.split_once(' ') else {
                continue;
            };
            let is_dir = path.ends_with('/');
            let path = path.trim_end_matches('/').to_string();
            // A manifest from before record_run checked its paths (review
            // N2): never delete or restore outside the unit.
            if !path.is_empty() && crate::paths::check_rel(&path).is_err() {
                u.unbacked.push(path);
                continue;
            }
            if let Some(was_dir) = first.get(&path) {
                if kind == "+" && is_dir != *was_dir {
                    delete.insert((path, is_dir));
                }
                continue;
            }
            first.insert(path.clone(), is_dir);
            match (kind, is_dir) {
                ("+", _) => {
                    delete.insert((path, is_dir));
                }
                // A directory's contents have lines of their own, and what it
                // was is in `dirs`.
                (_, true) => {}
                _ if run.backed_up.contains(&path) => u.restore[i].push(path),
                _ if run.absent.contains(&path) => {
                    delete.insert((path, false));
                }
                _ => u.unbacked.push(path),
            }
        }
        // Backed up but not reported (a run that died): restore it all.
        for f in &run.backed_up {
            if !first.contains_key(f) {
                first.insert(f.clone(), false);
                u.restore[i].push(f.clone());
            }
        }
        if !run.reported {
            u.unreported.push(i + 1);
        }
    }
    u.delete = delete.into_iter().collect();
    u.dirs.sort();
    u
}
