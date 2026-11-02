//! Home is also the backup (PLAN.md): before a push writes a unit, home
//! copies every file the push is about to replace or delete into
//! `backup/<host>/<YYYYMMDDTHHMMSSZ>/run-<n>/`, and records the push in a
//! manifest (`state/pushes/`): the unit, each run's backed-up files and
//! what rsync reported writing. That is what `replicant rollback` undoes.
//!
//! Home takes the backup itself rather than passing `--backup-dir` to the
//! push: macOS's openrsync, given `--backup`, silently skips `--delete`
//! (measured), so the push would leave every deleted file in place on such
//! a receiver. The list comes from the leaf's dry run of that very push,
//! taken just before it; what the push then wrote beyond it is recorded as
//! not backed up.
//!
//! A push that is resumed is the same push: each attempt is a run, so a
//! file the first run already replaced keeps its original copy in run 1.
//!
//! Backups older than fleet.toml's `[daemon] backup_days` (default 14) are
//! swept when home starts and once a day after (`home_sweep.rs`).

use super::Home;
use crate::marker::{self, Marker};
use crate::record::{RECORD_VERSION, Record, Store};
use crate::rsync::{self, Client, Runner};
use crate::{Error, Result, paths, timefmt};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The bound on home's own rsyncs (a backup copy, a rollback). A backup
/// runs inside the leaf's `BeginPush` request, which waits 15 minutes.
const HOME_RSYNC_TIMEOUT: Duration = Duration::from_secs(600);

/// One push of one unit, across its runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Push {
    pub version: u32,
    /// `<host>/<YYYYMMDDTHHMMSSZ>` (`-<n>` if that second was taken): the
    /// record's key, and its directory under `backup/`.
    pub id: String,
    pub host: String,
    pub unit: String,
    /// The unit is a single file, not a directory.
    #[serde(default)]
    pub is_file: bool,
    pub started_at: u64,
    pub runs: Vec<Run>,
    #[serde(default)]
    pub finished_at: Option<u64>,
    #[serde(default)]
    pub rolled_back_at: Option<u64>,
    /// When the pushing host and home agreed on the unit again after the
    /// rollback (review A2). Until then home reports the rollback to that
    /// host, whose sync holds the unit, and the sweep keeps the backup.
    #[serde(default)]
    pub rollback_settled_at: Option<u64>,
}

/// One rsync of a push: the first, or a resume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub started_at: u64,
    /// Unit-relative files copied into `run-<n>/` before the rsync wrote.
    pub backed_up: Vec<String>,
    /// Files the push said it would replace that did not exist yet (openrsync
    /// itemizes a file bound for a new directory as an update): created by
    /// the push, so a rollback deletes them.
    #[serde(default)]
    pub absent: Vec<String>,
    /// What rsync reported writing, in the dry run's notation (`~ a`,
    /// `+ d/`, `- b`); partial for a run that failed.
    pub changes: Vec<String>,
    /// Whether rsync's report arrived. A leaf that died mid-push never
    /// sends one, so what that run added is not known.
    pub reported: bool,
}

impl Record for Push {
    fn rel(&self) -> &str {
        &self.id
    }
    fn version(&self) -> u32 {
        self.version
    }
}

/// Home's push manifests: `state/pushes/`.
pub fn pushes(state_dir: &Path) -> Store {
    Store::at(PathBuf::from(state_dir).join("pushes"))
}

/// A push's id, checked: `<host>/<compact time>[-<n>]`, nothing that could
/// leave `backup/`.
pub(super) fn check_id(id: &str) -> Result<()> {
    let ok = id.split_once('/').is_some_and(|(host, when)| {
        crate::config::short_hostname(host).as_deref() == Some(host)
            && timefmt::parse_compact(when.split('-').next().unwrap_or("")).is_some()
            && when
                .split_once('-')
                .is_none_or(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    });
    match ok {
        true => Ok(()),
        false => Err(Error::Refused(format!(
            "{id:?} is not a push id (host/YYYYMMDDTHHMMSSZ)"
        ))),
    }
}

/// Where rsync's list operations on a unit run, and what the listed paths
/// are called there: the unit itself, or for a unit that is a single file,
/// its directory, with the file's name as the one entry.
pub(super) fn list_root(
    unit_path: &Path,
    is_file: bool,
    files: &[String],
) -> (PathBuf, Vec<String>) {
    match (is_file, unit_path.parent(), unit_path.file_name()) {
        (true, Some(dir), Some(name)) if !files.is_empty() => {
            (dir.to_path_buf(), vec![name.to_string_lossy().into_owned()])
        }
        _ => (unit_path.to_path_buf(), files.to_vec()),
    }
}

impl Home {
    pub(super) fn runner(&self) -> Result<Runner> {
        Ok(Runner::new(
            HOME_RSYNC_TIMEOUT,
            Client::detect()?,
            self.dirs.scratch(),
        ))
    }

    pub(super) fn push_dir(&self, id: &str) -> PathBuf {
        self.dirs.backup().join(id)
    }

    /// Back up `files` (unit-relative) of `rel` before `host` pushes it, as
    /// a new run of the push its marker names, or of a new push. Returns
    /// the push id. On failure nothing is recorded and the run's directory
    /// is removed.
    pub(super) fn back_up(
        &self,
        host: &str,
        rel: &str,
        unit_path: &Path,
        is_file: bool,
        files: &[String],
        resuming: Option<&Marker>,
    ) -> Result<String> {
        for f in files {
            if !f.is_empty() && paths::check_rel(f).is_err() {
                return Err(Error::Refused(format!(
                    "{f:?} is not a path inside {rel}; nothing backed up"
                )));
            }
        }
        let store = pushes(&self.dirs.state());
        let now = crate::now_secs();
        let mut push = match resuming.and_then(|m| m.push.clone()) {
            Some(id) => store.get::<Push>(&id)?.ok_or_else(|| {
                Error::Config(format!(
                    "the manifest of push {id} is missing; cannot resume it"
                ))
            })?,
            None => Push {
                version: RECORD_VERSION,
                id: self.new_push_id(host, now),
                host: host.into(),
                unit: rel.into(),
                is_file,
                started_at: now,
                runs: vec![],
                finished_at: None,
                rolled_back_at: None,
                rollback_settled_at: None,
            },
        };
        let run_dir = self
            .push_dir(&push.id)
            .join(format!("run-{}", push.runs.len() + 1));
        std::fs::create_dir_all(&run_dir)?;
        let exists = |f: &String| {
            let (root, names) = list_root(unit_path, push.is_file, std::slice::from_ref(f));
            std::fs::symlink_metadata(root.join(&names[0])).is_ok_and(|m| !m.is_dir())
        };
        let (present, absent): (Vec<String>, Vec<String>) =
            files.iter().cloned().partition(|f| exists(f));
        if !present.is_empty() {
            let (from, list) = list_root(unit_path, push.is_file, &present);
            let ran = rsync::copy_listed(&self.runner()?, &from, &run_dir, &list)?;
            if !ran.ok() {
                let _ = std::fs::remove_dir_all(&run_dir);
                return Err(Error::Rsync(format!(
                    "could not back up {rel} before the push ({}): {}",
                    ran.why(),
                    ran.stderr
                )));
            }
        }
        push.runs.push(Run {
            started_at: now,
            backed_up: present,
            absent,
            changes: vec![],
            reported: false,
        });
        store.put(&push)?;
        Ok(push.id)
    }

    /// A fresh id: `<host>/<now>`, or `-2`, `-3`… if taken.
    fn new_push_id(&self, host: &str, now: u64) -> String {
        let base = format!("{host}/{}", timefmt::compact(now));
        let mut id = base.clone();
        let mut n = 1;
        while self.push_dir(&id).exists() {
            n += 1;
            id = format!("{base}-{n}");
        }
        id
    }

    /// rsync's report for the current run of `host`'s push of `rel`: the
    /// changes it wrote, and whether the push is finished.
    pub(super) fn record_run(
        &self,
        host: &str,
        rel: &str,
        changes: &[String],
        finished: bool,
    ) -> Result<Option<String>> {
        // What the leaf says rsync wrote becomes rollback's delete list
        // (review N2): every path must stay inside the unit, as the backup
        // list must (empty: a single-file unit itself).
        for c in changes {
            let path = c
                .split_once(' ')
                .map_or("", |(_, p)| p.trim_end_matches('/'));
            if !path.is_empty() && paths::check_rel(path).is_err() {
                return Err(Error::Refused(format!(
                    "{c:?} is not a change inside {rel}; nothing recorded"
                )));
            }
        }
        let m: Option<Marker> = marker::store(&self.dirs.state()).get(rel)?;
        let Some(id) = m.filter(|m| m.host == host).and_then(|m| m.push) else {
            return Ok(None);
        };
        let store = pushes(&self.dirs.state());
        let Some(mut push) = store.get::<Push>(&id)? else {
            return Ok(None);
        };
        if let Some(run) = push.runs.last_mut() {
            run.changes = changes.to_vec();
            run.reported = true;
        }
        if finished {
            push.finished_at = Some(crate::now_secs());
        }
        store.put(&push)?;
        Ok(Some(id))
    }
}
