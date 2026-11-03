//! `replicant rollback <push-id>` on home: undo one push of one unit.
//! PLAN.md, "Interrupted sync" and "Home is also the backup".
//!
//! Each path the push touched is put back as it was before the push's
//! first run touched it: a file that run replaced or deleted comes back from
//! that run's backup; a path it created is deleted; a directory it removed or
//! changed the mode of comes back with the mode it had (`native::local`).
//! What each run backed up is what its directory holds, read again here,
//! with the directories it noted. What the push holds now of those paths
//! is first copied to `<push>/rollback/`, so a rollback can be undone by
//! hand. Baselines are untouched: the pushing leaf next sees home changed,
//! and holds the unit as a conflict until it accepts the rollback and
//! pulls the restored state (review A2, `home_rolled_back.rs`).
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
use super::backup::{Push, check_id, kept, list_root, pushes};
use crate::daemon::journal::Line;
use crate::daemon::queue::EntryKind;
use crate::daemon::wire::RolledBack;
use crate::marker::{self, Marker};
use crate::native::local;
use crate::{Error, Result};
use std::collections::BTreeSet;
use std::sync::mpsc;

#[path = "home_rollback_undo.rs"]
mod plan;
pub(super) use plan::undo;

fn ok<T>(r: Result<T>, what: &str) -> Result<T> {
    r.map_err(|e| Error::Transfer(format!("rollback: {what} failed: {e}")))
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
            detail.push_str("; a run never reported on may have added files that stay");
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
        let dir = self.push_dir(&push.id);
        // Each run's directory is the record of what it kept.
        let mut push = push.clone();
        for (i, run) in push.runs.iter_mut().enumerate() {
            let run_dir = dir.join(format!("run-{}", i + 1));
            if run_dir.is_dir() {
                run.backed_up = kept(&run_dir, &unit_path, push.is_file)?;
                run.dirs = crate::native::backup::dirs_noted(&run_dir)?;
            }
        }
        let push = &push;
        let u = undo(push);
        let at = |files: &[String]| list_root(&unit_path, push.is_file, files);

        // A directory to bring back where a run that never reported put a
        // file or link: nothing else writes here while the queue is held.
        let mut delete = u.delete.clone();
        delete.extend(
            u.dirs
                .iter()
                .filter(|(d, _)| local::holds_file(&unit_path, d))
                .map(|(d, _)| (d.clone(), false)),
        );

        // What the push holds now of every path about to change.
        let now: Vec<String> = u
            .restore
            .iter()
            .flatten()
            .chain(delete.iter().filter(|(_, d)| !d).map(|(p, _)| p))
            .filter(|p| {
                let (root, names) = at(std::slice::from_ref(p));
                local::holds_file(&root, &names[0])
            })
            .cloned()
            .collect::<BTreeSet<String>>()
            .into_iter()
            .collect();
        if !now.is_empty() {
            let kept = dir.join("rollback");
            std::fs::create_dir_all(&kept)?;
            let (from, list) = at(&now);
            ok(
                local::copy_listed(&from, &kept, &list),
                "keeping the pushed copies",
            )?;
        }
        if !delete.is_empty() {
            let (root, names) = at(&delete.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>());
            let delete: Vec<(String, bool)> = match push.is_file {
                true => names.into_iter().map(|n| (n, false)).collect(),
                false => delete,
            };
            ok(
                local::delete_listed(&root, &delete),
                "deleting what it created",
            )?;
        }
        ok(
            local::open_dirs(&unit_path, &u.dirs),
            "bringing its directories back",
        )?;
        for (i, files) in u.restore.iter().enumerate().filter(|(_, f)| !f.is_empty()) {
            let run_dir = dir.join(format!("run-{}", i + 1));
            let (to, list) = at(files);
            ok(local::copy_listed(&run_dir, &to, &list), "restoring")?;
        }
        ok(
            local::set_dir_modes(&unit_path, &u.dirs),
            "restoring directory modes",
        )?;
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
