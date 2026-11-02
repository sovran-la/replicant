//! The backup sweep: backups older than fleet.toml's `[daemon] backup_days`
//! (default 14) go, when home starts and once a day after, except a push's
//! whose marker is still open, and a push home rolled back that its host
//! has not settled yet (review A2): what the rollback undid is the leaf's
//! work, kept in `<push>/rollback/`, and the manifest is what holds the
//! leaf's next sync.

use super::Home;
use super::backup::{Push, check_id, pushes};
use crate::Result;
use crate::daemon::journal::Line;
use crate::marker::{self, Marker};
use crate::timefmt;

impl Home {
    /// Delete backups older than fleet.toml's `backup_days`, except those
    /// whose push still has a marker open or an unsettled rollback. Returns
    /// how many went.
    pub fn sweep_backups(&self, now: u64) -> Result<usize> {
        let keep = self.fleet()?.backup_keep.as_secs();
        let mut open: Vec<String> = marker::store(&self.dirs.state())
            .all::<Marker>()?
            .into_iter()
            .filter_map(|m| m.push)
            .collect();
        let store = pushes(&self.dirs.state());
        open.extend(
            store
                .all::<Push>()?
                .into_iter()
                .filter(|p| p.rolled_back_at.is_some() && p.rollback_settled_at.is_none())
                .map(|p| p.id),
        );
        let mut gone = 0;
        let hosts = match std::fs::read_dir(self.dirs.backup()) {
            Ok(h) => h,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        for h in hosts {
            let h = h?;
            let Some(host) = h.file_name().to_str().map(String::from) else {
                continue;
            };
            if !h.file_type()?.is_dir() {
                continue;
            }
            for d in std::fs::read_dir(h.path())? {
                let d = d?;
                let Some(name) = d.file_name().to_str().map(String::from) else {
                    continue;
                };
                let id = format!("{host}/{name}");
                let Some(at) = timefmt::parse_compact(name.split('-').next().unwrap_or("")) else {
                    continue;
                };
                if check_id(&id).is_err() || now.saturating_sub(at) < keep || open.contains(&id) {
                    continue;
                }
                std::fs::remove_dir_all(d.path())?;
                store.delete(&id)?;
                gone += 1;
            }
        }
        if gone > 0 {
            let mut j = Vec::new();
            let detail = format!(
                "removed {gone} push backups older than {} days",
                keep / 86_400
            );
            self.journal(&mut j, Line::new("backup-sweep", &self.name()?, "", detail));
        }
        Ok(gone)
    }
}
