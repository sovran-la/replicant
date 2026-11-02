//! What home's own CLI reads: the fleet, pending hosts, an allow preview,
//! the journal; and the reaper's sweep of dead queue entries.

use super::Home;
use crate::daemon::fleet::{self, FleetState, PendingView};
use crate::daemon::journal::{Filter, Line};
use crate::daemon::wire::AllowPreview;
use crate::marker::{self, Marker};
use crate::{Result, subscribe};
use std::time::Instant;

impl Home {
    pub fn fleet_state(&self) -> Result<FleetState> {
        let markers = marker::store(&self.dirs.state()).all::<Marker>()?;
        let f = self.fleet()?;
        fleet::build(
            &f,
            &self.contacts,
            self.queue.view(),
            markers,
            crate::now_secs(),
        )
    }

    pub fn pending(&self) -> Result<Vec<PendingView>> {
        Ok(fleet::pending_views(&*self.fleet()?))
    }

    pub fn preview_allow(&self, host: &str, raw: &[String]) -> Result<AllowPreview> {
        subscribe::preview_allow(&*self.fleet()?, host, raw)
    }

    pub fn log(&self, filter: &Filter) -> Result<Vec<Line>> {
        self.journal.read(filter)
    }

    /// Expire dead queue entries, journaling each one loudly.
    pub fn reap(&self) {
        for e in self.queue.expire(Instant::now()) {
            let mut ignore = Vec::new();
            let detail = format!(
                "lease expired: {} (queue released; markers untouched)",
                crate::daemon::queue::describe(&e)
            );
            self.journal(
                &mut ignore,
                Line::new("expired", &e.host, &e.paths.join(" "), detail),
            );
        }
    }
}
