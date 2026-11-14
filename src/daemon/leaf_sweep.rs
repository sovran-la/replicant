//! The leaf's backup sweep (rev 22): what a resolved pull replaced here is
//! kept under `backup/resolved/<when>/<rel>` for [`KEEP_DAYS`], as home keeps
//! a push's by default, and swept at the start of the next `sync` after
//! that. A run an open marker still names (an interrupted resolved pull,
//! which its resume keeps writing into) is never swept.

use crate::daemon::journal::{Journal, Line};
use crate::marker::{self, Marker};
use crate::plan::apply::RESOLVED;
use crate::{Result, timefmt};
use std::path::Path;

/// How long a resolved pull's backup stays, in days: home's default.
pub const KEEP_DAYS: u64 = crate::config::fleet::DEFAULT_BACKUP_DAYS;

/// Remove resolved-pull backup runs under `backup_dir` older than
/// [`KEEP_DAYS`] at `now`, except those an open marker in `state_dir`
/// names; journal how many went. Returns that count.
pub fn sweep(
    backup_dir: &Path,
    state_dir: &Path,
    journal: &Journal,
    me: &str,
    now: u64,
) -> Result<usize> {
    let root = backup_dir.join(RESOLVED);
    let runs = match std::fs::read_dir(&root) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.into()),
    };
    let open: Vec<_> = marker::store(state_dir)
        .all::<Marker>()?
        .into_iter()
        .filter_map(|m| m.backup)
        .collect();
    let keep = KEEP_DAYS * 86_400;
    let mut gone = 0;
    for run in runs {
        let run = run?;
        if !run.file_type()?.is_dir() {
            continue;
        }
        let Some(at) = run.file_name().to_str().and_then(timefmt::parse_compact) else {
            continue;
        };
        let path = run.path();
        if now.saturating_sub(at) < keep || open.iter().any(|b| b.starts_with(&path)) {
            continue;
        }
        std::fs::remove_dir_all(&path)?;
        gone += 1;
    }
    if gone > 0 {
        let detail = format!("removed {gone} resolved-pull backups older than {KEEP_DAYS} days");
        journal.append(&Line::new("backup-sweep", me, "", &detail))?;
    }
    Ok(gone)
}

#[cfg(test)]
#[path = "leaf_sweep_tests.rs"]
mod tests;
