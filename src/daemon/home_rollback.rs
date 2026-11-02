//! `replicant rollback <push-id>` on home: undo one push of one unit.
//! PLAN.md, "Interrupted sync" and "Home is also the backup".
//!
//! Each path the push touched is put back as it was before the push's
//! first run touched it: a file that run replaced or deleted comes back from
//! that run's backup; a path it created is deleted. Both through rsync
//! (`copy_listed`, `delete_listed`). What the push holds now of those paths
//! is first copied to `<push>/rollback/`, so a rollback can be undone by
//! hand. Baselines are untouched: the pushing leaf next sees home changed
//! and pulls the restored state (with guardrail 1 guarding its own work).
//!
//! It refuses a push already rolled back, one whose backup was swept, one
//! with a later push of the same unit that is not rolled back (undo the
//! later one first), and one with another sync's marker over its unit. (A
//! loose unit and the repos inside it are different units with different
//! files, so pushes of them are independent.) It
//! takes a queue entry, so nothing else runs on home while it writes, and
//! it clears the push's own marker: rollback is one of the three ways a
//! quarantine ends.

use super::Home;
use super::backup::{Push, check_id, list_root, pushes};
use crate::daemon::journal::Line;
use crate::daemon::queue::EntryKind;
use crate::daemon::wire::RolledBack;
use crate::marker::{self, Marker};
use crate::rsync::{self, Ran};
use crate::{Error, Result};
use std::collections::BTreeSet;
use std::sync::mpsc;

/// What undoing a push takes: per run, the files to restore from it; the
/// paths to delete; the files nothing was backed up for.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Undo {
    pub restore: Vec<Vec<String>>,
    pub delete: Vec<(String, bool)>,
    pub unbacked: Vec<String>,
    /// Runs rsync never reported on: what they created is not known.
    pub unreported: Vec<usize>,
}

/// Each path decided by the first run that touched it.
pub(super) fn undo(push: &Push) -> Undo {
    let mut u = Undo {
        restore: vec![vec![]; push.runs.len()],
        ..Default::default()
    };
    let mut seen = BTreeSet::new();
    for (i, run) in push.runs.iter().enumerate() {
        for c in &run.changes {
            let Some((kind, path)) = c.split_once(' ') else {
                continue;
            };
            let is_dir = path.ends_with('/');
            let path = path.trim_end_matches('/').to_string();
            if !seen.insert(path.clone()) {
                continue;
            }
            match (kind, is_dir) {
                ("+", _) => u.delete.push((path, is_dir)),
                // A directory's contents have lines of their own.
                (_, true) => {}
                _ if run.backed_up.contains(&path) => u.restore[i].push(path),
                _ if run.absent.contains(&path) => u.delete.push((path, false)),
                _ => u.unbacked.push(path),
            }
        }
        // Backed up but not reported (a run that died): restore it all.
        for f in &run.backed_up {
            if seen.insert(f.clone()) {
                u.restore[i].push(f.clone());
            }
        }
        if !run.reported {
            u.unreported.push(i + 1);
        }
    }
    u.delete.sort();
    u
}

fn ok(ran: Ran, what: &str) -> Result<Ran> {
    match ran.ok() {
        true => Ok(ran),
        false => Err(Error::Rsync(format!(
            "rollback: {what} failed ({}): {}",
            ran.why(),
            ran.stderr
        ))),
    }
}

impl Home {
    /// Every push home has a manifest for, newest first.
    pub fn pushes(&self) -> Result<Vec<Push>> {
        let mut all: Vec<Push> = pushes(&self.dirs.state()).all()?;
        all.sort_by(|a, b| b.started_at.cmp(&a.started_at).then(b.id.cmp(&a.id)));
        Ok(all)
    }

    pub fn rollback(&self, id: &str) -> Result<RolledBack> {
        check_id(id)?;
        let store = pushes(&self.dirs.state());
        let mut push: Push = store
            .get(id)?
            .ok_or_else(|| Error::Refused(format!("no push {id} on home")))?;
        if let Some(at) = push.rolled_back_at {
            return Err(Error::Refused(format!(
                "push {id} was already rolled back at {}",
                crate::timefmt::iso(at)
            )));
        }
        if !self.push_dir(id).is_dir() {
            return Err(Error::Refused(format!(
                "push {id}'s backup is gone (swept after backup_days?); nothing to restore from"
            )));
        }
        let later: Vec<String> = self
            .pushes()?
            .into_iter()
            .filter(|p| p.id != id && p.unit == push.unit && p.rolled_back_at.is_none())
            .filter(|p| (p.started_at, &p.id) > (push.started_at, &push.id))
            .map(|p| p.id)
            .collect();
        if !later.is_empty() {
            return Err(Error::Refused(format!(
                "{} pushed {} again after {id}: roll back {} first",
                push.host,
                push.unit,
                later.join(", ")
            )));
        }

        let (f_name, lease) = {
            let f = self.fleet()?;
            (f.name.clone(), f.lease)
        };
        let entry = self.queue.enqueue(
            &f_name,
            EntryKind::Rollback,
            vec![push.unit.clone()],
            lease,
            false,
        )?;
        self.queue
            .wait(entry, lease, super::MAX_QUEUE_WAIT, &|| true)?;
        let (stop, stopped) = mpsc::channel::<()>();
        let result = std::thread::scope(|s| {
            s.spawn(move || {
                while let Err(mpsc::RecvTimeoutError::Timeout) = stopped.recv_timeout(lease / 3) {
                    let _ = self.queue.heartbeat(entry);
                }
            });
            let r = self.undo_push(&push);
            drop(stop);
            r
        });
        let _ = self.queue.release(entry);
        let (restored, removed, unbacked, unreported) = result?;

        push.rolled_back_at = Some(crate::now_secs());
        store.put(&push)?;
        let mut detail = format!("push {id}: restored {restored}, removed {removed}");
        if !unbacked.is_empty() {
            detail.push_str(&format!(
                "; not backed up, left as pushed: {}",
                unbacked.join(" ")
            ));
        }
        if !unreported.is_empty() {
            detail.push_str("; a run rsync never reported on may have added files that stay");
        }
        let mut j = Vec::new();
        self.journal(&mut j, Line::new("rollback", &f_name, &push.unit, detail));
        Ok(RolledBack {
            id: id.into(),
            unit: push.unit,
            host: push.host,
            restored,
            removed,
            unbacked,
            unreported,
            kept: self.push_dir(id).join("rollback"),
        })
    }

    /// The writes: keep what is there now, delete what the push created,
    /// restore what it replaced, clear its marker.
    fn undo_push(&self, push: &Push) -> Result<(usize, usize, Vec<String>, Vec<usize>)> {
        let markers = marker::store(&self.dirs.state());
        let over: Option<Marker> = markers
            .all::<Marker>()?
            .into_iter()
            .find(|m| m.covers(&push.unit) && m.push.as_deref() != Some(push.id.as_str()));
        if let Some(m) = over {
            return Err(Error::Refused(format!(
                "an interrupted {} of {} by {} is marked over {}; roll that back or let it \
                 resume first",
                m.direction, m.rel, m.host, push.unit
            )));
        }
        let unit_path = self.fleet()?.home_dir.join(&push.unit);
        let u = undo(push);
        let runner = self.runner()?;
        let dir = self.push_dir(&push.id);
        let at = |files: &[String]| list_root(&unit_path, push.is_file, files);

        // What the push holds now of every path about to change.
        let now: Vec<String> = u
            .restore
            .iter()
            .flatten()
            .chain(u.delete.iter().filter(|(_, d)| !d).map(|(p, _)| p))
            .filter(|p| {
                let (root, names) = at(std::slice::from_ref(p));
                std::fs::symlink_metadata(root.join(&names[0])).is_ok()
            })
            .cloned()
            .collect();
        if !now.is_empty() {
            let kept = dir.join("rollback");
            std::fs::create_dir_all(&kept)?;
            let (from, list) = at(&now);
            ok(
                rsync::copy_listed(&runner, &from, &kept, &list)?,
                "keeping the pushed copies",
            )?;
        }
        if !u.delete.is_empty() {
            let (root, names) = at(&u.delete.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>());
            let delete: Vec<(String, bool)> = match push.is_file {
                true => names.into_iter().map(|n| (n, false)).collect(),
                false => u.delete.clone(),
            };
            ok(
                rsync::delete_listed(&runner, None, &root, &delete)?,
                "deleting what it created",
            )?;
        }
        for (i, files) in u.restore.iter().enumerate().filter(|(_, f)| !f.is_empty()) {
            let run_dir = dir.join(format!("run-{}", i + 1));
            let (to, list) = at(files);
            ok(
                rsync::copy_listed(&runner, &run_dir, &to, &list)?,
                "restoring",
            )?;
        }
        if let Some(m) = markers.get::<Marker>(&push.unit)?
            && m.push.as_deref() == Some(push.id.as_str())
        {
            markers.delete(&push.unit)?;
        }
        let restored = u.restore.iter().map(Vec::len).sum();
        Ok((restored, u.delete.len(), u.unbacked, u.unreported))
    }
}

#[cfg(test)]
#[path = "home_rollback_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "home_rollback_hold_tests.rs"]
mod hold_tests;
