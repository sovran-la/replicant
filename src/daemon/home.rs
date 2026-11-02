//! Home's request handling: everything a leaf can ask, and what home's own
//! CLI asks (`approve`, `status`, `log`).
//!
//! One mutex guards the fleet: every write to fleet.toml happens under it and
//! is followed by a reload, so concurrent subscribes cannot lose each other's
//! edits. Every look also reloads after a hand edit ([`cache`]), so every
//! reader sees what the file says.

use super::fleet::{self, FleetState, LastReport, PendingView};
use super::journal::{Filter, Journal, Line};
use super::queue::{EntryKind, Queue};
use super::wire::*;
use crate::config::{Dirs, short_hostname};
use crate::marker::{self, Marker};
use crate::record::Store;
use crate::{Error, Result, subscribe};
use std::time::{Duration, Instant};

/// How long a dry run may wait in the queue before giving up.
pub const MAX_QUEUE_WAIT: Duration = Duration::from_secs(600);

pub struct Home {
    pub dirs: Dirs,
    fleet: cache::FleetCache,
    pub queue: Queue,
    pub journal: Journal,
    contacts: Store,
    /// The bootstrap bundle's fingerprint (`None` only if it vanished
    /// between [`Home::open`]'s generate and read).
    bootstrap: Option<String>,
}

impl Home {
    /// Open home, generating its CA, server identity and bootstrap bundle
    /// on a first start.
    pub fn open(dirs: Dirs) -> Result<Self> {
        super::certs::ensure(&super::certs::Paths::of(&dirs))?;
        Ok(Self {
            bootstrap: super::certs::bootstrap_fingerprint(&dirs.credentials())?,
            journal: Journal::at(dirs.journal()),
            contacts: fleet::contacts(&dirs.state()),
            fleet: cache::FleetCache::open(dirs.fleet())?,
            queue: Queue::new(),
            dirs,
        })
    }

    /// The fleet as fleet.toml says it is now (reloaded after a hand edit).
    fn fleet(&self) -> Result<cache::FleetGuard<'_>> {
        self.fleet.get()
    }

    /// Reload after a write, under the same guard that wrote.
    fn reload(&self, guard: &mut cache::FleetGuard<'_>) -> Result<()> {
        guard.reload()
    }

    pub fn name(&self) -> Result<String> {
        Ok(self.fleet()?.name.clone())
    }

    fn journal(&self, out: &mut Vec<String>, line: Line) {
        match self.journal.append(&line) {
            Ok(text) => out.push(text),
            Err(e) => eprintln!("replicant: could not journal {:?}: {e}", line.event),
        }
    }

    /// Handle one request in-process, as the host it names, from a caller
    /// that is always connected. Tests only: nothing in-process is a leaf.
    #[cfg(test)]
    pub fn handle(&self, call: HomeCall) -> HomeAnswer {
        self.answer(call, &Caller::Trusted, &|| true)
    }

    /// Handle one leaf request from the TLS peer whose certificate has
    /// fingerprint `peer`. `connected` says whether the leaf is still on the
    /// line; a queue wait refreshes its lease only while it is.
    pub fn handle_from(
        &self,
        call: HomeCall,
        peer: &str,
        connected: &dyn Fn() -> bool,
    ) -> HomeAnswer {
        self.answer(call, &Caller::Cert(peer.into()), connected)
    }

    pub(super) fn answer(
        &self,
        call: HomeCall,
        caller: &Caller,
        connected: &dyn Fn() -> bool,
    ) -> HomeAnswer {
        let mut journal = Vec::new();
        let result = self
            .dispatch(call, caller, connected, &mut journal)
            .map_err(|e| e.to_string());
        HomeAnswer { result, journal }
    }

    fn dispatch(
        &self,
        call: HomeCall,
        caller: &Caller,
        connected: &dyn Fn() -> bool,
        j: &mut Vec<String>,
    ) -> Result<HomeReply> {
        if call.version != WIRE_VERSION {
            return Err(Error::Remote(format!(
                "request is wire version {}, this home speaks {WIRE_VERSION}; run the same replicant on both",
                call.version
            )));
        }
        let claimed = short_hostname(&call.host)
            .filter(|h| *h == call.host)
            .ok_or_else(|| Error::Refused(format!("{:?} is not a valid host name", call.host)))?;
        if !call.home_dir.is_absolute() {
            return Err(Error::Refused("home_dir must be absolute".into()));
        }
        let Admitted { host, bootstrap } = self.admit(caller, &claimed, &call.request)?;
        fleet::touch(&self.contacts, &host, None)?;
        let file = self.dirs.fleet();
        match call.request {
            HomeRequest::Subscribe { paths } => {
                let mut f = self.fleet()?;
                if bootstrap {
                    self.bootstrap_may_subscribe(&f, &host)?;
                }
                let out = subscribe::subscribe(&f, &file, &host, &call.home_dir, &paths)?;
                self.reload(&mut f)?;
                let (event, detail) = if out.enrolled {
                    ("subscribe", format!("now: {}", out.paths.join(" ")))
                } else {
                    (
                        "pending",
                        format!(
                            "home_dir:{} wants: {}",
                            call.home_dir.display(),
                            out.paths.join(" ")
                        ),
                    )
                };
                self.journal(j, Line::new(event, &host, &out.requested.join(" "), detail));
                Ok(HomeReply::Subscribed(out))
            }
            HomeRequest::Unsubscribe { paths } => {
                let mut f = self.fleet()?;
                let out = subscribe::unsubscribe(&f, &file, &host, &call.home_dir, &paths)?;
                self.reload(&mut f)?;
                let what: Vec<String> = out.removed.iter().chain(&out.carved).cloned().collect();
                self.journal(
                    j,
                    Line::new(
                        "unsubscribe",
                        &host,
                        &what.join(" "),
                        format!("now: {}", out.paths.join(" ")),
                    ),
                );
                Ok(HomeReply::Unsubscribed(out))
            }
            HomeRequest::Subscriptions => Ok(HomeReply::Subscriptions(self.view(&host)?)),
            HomeRequest::Acquire { no_wait } => {
                let (specs, lease) = {
                    let f = self.fleet()?;
                    (self.served(&f, &host)?.0, f.lease)
                };
                let paths: Vec<String> = specs.iter().map(|s| s.rel.clone()).collect();
                let what = paths.join(" ");
                let id = self
                    .queue
                    .enqueue(&host, EntryKind::DryRun, paths, lease, no_wait)?;
                if let Err(e) = self.queue.wait(id, lease, MAX_QUEUE_WAIT, connected) {
                    // Withdrawn at the head: as loud as a sweep's expiry.
                    if e.to_string().contains(super::queue::WAITER_GONE) {
                        let detail = format!("{e} (queue released; markers untouched)");
                        self.journal(j, Line::new("expired", &host, &what, detail));
                    }
                    return Err(e);
                }
                Ok(HomeReply::Lease {
                    lease: id,
                    lease_secs: lease.as_secs(),
                })
            }
            // A lease lives on what it was granted, so these read no policy:
            // a broken fleet.toml must not abort syncs in flight (review P5).
            // Everything that reads policy fails closed.
            HomeRequest::Heartbeat { lease } => {
                self.queue.heartbeat(lease)?;
                Ok(HomeReply::Done)
            }
            HomeRequest::Release { lease } => {
                self.queue.release(lease)?;
                Ok(HomeReply::Done)
            }
            HomeRequest::State {
                leaf_repos,
                case_check,
                baselines,
            } => Ok(HomeReply::State(self.snapshot(
                &host,
                &leaf_repos,
                case_check,
                &baselines,
            )?)),
            HomeRequest::Report {
                event,
                units,
                summary,
            } => {
                let paths: Vec<String> =
                    self.view(&host)?.specs.into_iter().map(|s| s.rel).collect();
                let report = LastReport {
                    at: crate::now_secs(),
                    event: event.clone(),
                    units,
                };
                fleet::touch(&self.contacts, &host, Some(report))?;
                self.journal(j, Line::new(&event, &host, &paths.join(" "), summary));
                Ok(HomeReply::Done)
            }
            HomeRequest::FleetState => Ok(HomeReply::Fleet(self.fleet_state()?)),
        }
    }

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
                super::queue::describe(&e)
            );
            self.journal(
                &mut ignore,
                Line::new("expired", &e.host, &e.paths.join(" "), detail),
            );
        }
    }
}

#[path = "home_fleet.rs"]
mod cache;

#[path = "home_trust.rs"]
mod trust;

#[path = "home_enroll.rs"]
mod enroll;
use trust::Admitted;
pub use trust::{BOOTSTRAP_ONLY, Caller};

#[path = "home_serve.rs"]
mod serve;

#[cfg(test)]
#[path = "home_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "home_reload_tests.rs"]
mod reload_tests;
