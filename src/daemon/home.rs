//! Home's request handling: everything a leaf can ask, and what home's own
//! CLI asks (`approve`, `status`, `log`).
//!
//! One mutex guards the fleet: every write to fleet.toml happens under it and
//! is followed by a reload, so concurrent subscribes cannot lose each other's
//! edits and every reader sees what the file says.

use super::fleet::{self, FleetState, LastReport, PendingView};
use super::journal::{Filter, Journal, Line};
use super::queue::{EntryKind, Queue};
use super::wire::*;
use crate::config::{Dirs, Fleet, short_hostname};
use crate::guard::{self, Refusal};
use crate::marker::{self, Marker};
use crate::record::Store;
use crate::units::{self, Discovered, SubSpec};
use crate::{Error, Result, paths, subscribe};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How long a dry run may wait in the queue before giving up.
pub const MAX_QUEUE_WAIT: Duration = Duration::from_secs(600);

pub struct Home {
    pub dirs: Dirs,
    fleet: Mutex<Fleet>,
    pub queue: Queue,
    pub journal: Journal,
    contacts: Store,
}

impl Home {
    pub fn open(dirs: Dirs) -> Result<Self> {
        let fleet = Fleet::load(&dirs.fleet())?;
        Ok(Self {
            journal: Journal::at(dirs.journal()),
            contacts: fleet::contacts(&dirs.state()),
            fleet: Mutex::new(fleet),
            queue: Queue::new(),
            dirs,
        })
    }

    fn fleet(&self) -> MutexGuard<'_, Fleet> {
        self.fleet.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Reload after a write, under the same guard that wrote.
    fn reload(&self, guard: &mut MutexGuard<'_, Fleet>) -> Result<()> {
        **guard = Fleet::load(&self.dirs.fleet())?;
        Ok(())
    }

    pub fn name(&self) -> String {
        self.fleet().name.clone()
    }

    fn journal(&self, out: &mut Vec<String>, line: Line) {
        match self.journal.append(&line) {
            Ok(text) => out.push(text),
            Err(e) => eprintln!("replicant: could not journal {:?}: {e}", line.event),
        }
    }

    /// Handle one leaf request.
    pub fn handle(&self, call: HomeCall) -> HomeAnswer {
        let mut journal = Vec::new();
        let result = self.dispatch(call, &mut journal).map_err(|e| e.to_string());
        HomeAnswer { result, journal }
    }

    fn dispatch(&self, call: HomeCall, j: &mut Vec<String>) -> Result<HomeReply> {
        if call.version != WIRE_VERSION {
            return Err(Error::Remote(format!(
                "request is wire version {}, this home speaks {WIRE_VERSION}; run the same replicant on both",
                call.version
            )));
        }
        let host = short_hostname(&call.host)
            .filter(|h| *h == call.host)
            .ok_or_else(|| Error::Refused(format!("{:?} is not a valid host name", call.host)))?;
        if !call.home_dir.is_absolute() {
            return Err(Error::Refused("home_dir must be absolute".into()));
        }
        fleet::touch(&self.contacts, &host, None)?;
        let file = self.dirs.fleet();
        match call.request {
            HomeRequest::Subscribe { paths } => {
                let mut f = self.fleet();
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
                let mut f = self.fleet();
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
            HomeRequest::Subscriptions => Ok(HomeReply::Subscriptions(self.view(&host))),
            HomeRequest::Acquire { no_wait } => {
                let (specs, lease) = {
                    let f = self.fleet();
                    (self.served(&f, &host)?.0, f.lease)
                };
                let paths = specs.iter().map(|s| s.rel.clone()).collect();
                let id = self.queue.acquire(
                    &host,
                    EntryKind::DryRun,
                    paths,
                    lease,
                    no_wait,
                    MAX_QUEUE_WAIT,
                )?;
                Ok(HomeReply::Lease {
                    lease: id,
                    lease_secs: lease.as_secs(),
                })
            }
            HomeRequest::Heartbeat { lease } => {
                self.queue.heartbeat(lease, self.fleet().lease)?;
                Ok(HomeReply::Done)
            }
            HomeRequest::Release { lease } => {
                self.queue.release(lease)?;
                Ok(HomeReply::Done)
            }
            HomeRequest::State {
                leaf_repos,
                case_check,
            } => Ok(HomeReply::State(self.snapshot(
                &host,
                &leaf_repos,
                case_check,
            )?)),
            HomeRequest::Report {
                event,
                units,
                summary,
            } => {
                let paths: Vec<String> =
                    self.view(&host).specs.into_iter().map(|s| s.rel).collect();
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

    /// What `host` may be served: in-bounds specs, and refusals for the rest.
    fn served(&self, f: &Fleet, host: &str) -> Result<(Vec<SubSpec>, Vec<Refusal>)> {
        let Some(entry) = f.hosts.get(host) else {
            return Err(Error::Refused(if f.pending.contains_key(host) {
                format!(
                    "{host} is pending approval; run `replicant approve` on {}",
                    f.name
                )
            } else {
                format!(
                    "{host} is not enrolled on {}; run `replicant subscribe {} <paths>`",
                    f.name, f.name
                )
            }));
        };
        let sub = f.subscription(host);
        let (mut specs, mut refused) = (Vec::new(), Vec::new());
        for p in &sub.paths {
            let root = f.root_for(p);
            let cap_refusal = guard::bounds(p, &sub.paths, &[], entry.allow.as_deref());
            match (root, cap_refusal) {
                (Some(r), None) => specs.push(SubSpec {
                    rel: p.clone(),
                    exclude: r.exclude.clone(),
                    except: sub
                        .except
                        .iter()
                        .filter(|e| paths::is_within(e, p))
                        .cloned()
                        .collect(),
                }),
                (None, _) => refused.push(Refusal {
                    guard: guard::Guard::SubscriptionBounds,
                    unit: p.clone(),
                    detail: vec!["no longer under any root home serves".into()],
                }),
                (_, Some(r)) => refused.push(r),
            }
        }
        Ok((specs, refused))
    }

    fn view(&self, host: &str) -> SubscriptionsView {
        let f = self.fleet();
        let base = SubscriptionsView {
            home: f.name.clone(),
            enrollment: Enrollment::Unknown,
            specs: vec![],
            cap: None,
            refused: vec![],
            pending_paths: vec![],
            lease_secs: f.lease.as_secs(),
        };
        match self.served(&f, host) {
            Ok((specs, refused)) => SubscriptionsView {
                enrollment: Enrollment::Enrolled,
                specs,
                refused,
                cap: f.hosts[host].allow.clone(),
                ..base
            },
            Err(_) => match f.pending.get(host) {
                Some(p) => SubscriptionsView {
                    enrollment: Enrollment::Pending,
                    pending_paths: p.paths.clone(),
                    ..base
                },
                None => base,
            },
        }
    }

    /// Home's side of every unit `host` is subscribed to.
    fn snapshot(
        &self,
        host: &str,
        leaf_repos: &[String],
        case_check: bool,
    ) -> Result<HomeSnapshot> {
        let (specs, home_dir) = {
            let f = self.fleet();
            (self.served(&f, host)?.0, f.home_dir.clone())
        };
        let hd = home_dir.clone();
        let found = Discovered::run(&specs, &move |rel| hd.join(rel))?;
        let mut union = found.repos();
        // Guardrail 5 on home: a leaf's repo list cannot steer home outside
        // the host's bounds. Anything malformed or out of bounds is ignored.
        for r in leaf_repos {
            let ok = paths::check_rel(r).is_ok()
                && specs.iter().any(|s| {
                    paths::is_within(r, &s.rel) && !s.except.iter().any(|e| paths::is_within(r, e))
                });
            if ok && !union.contains(r) {
                union.push(r.clone());
            }
        }
        union.sort();
        let units = units::snapshot(&found, &union, case_check)?;
        let markers: Vec<Marker> = marker::store(&self.dirs.state())
            .all::<Marker>()?
            .into_iter()
            .filter(|m| specs.iter().any(|s| m.covers(&s.rel)))
            .collect();
        Ok(HomeSnapshot {
            home_dir,
            specs,
            units,
            repos: found.repos(),
            worktrees: found.worktrees(),
            markers,
        })
    }

    pub fn fleet_state(&self) -> Result<FleetState> {
        let markers = marker::store(&self.dirs.state()).all::<Marker>()?;
        let f = self.fleet();
        fleet::build(
            &f,
            &self.contacts,
            self.queue.view(),
            markers,
            crate::now_secs(),
        )
    }

    pub fn pending(&self) -> Vec<PendingView> {
        fleet::pending_views(&self.fleet())
    }

    pub fn preview_allow(&self, host: &str, raw: &[String]) -> Result<AllowPreview> {
        subscribe::preview_allow(&self.fleet(), host, raw)
    }

    pub fn approve(&self, host: &str, raw: &[String]) -> Result<Approved> {
        let mut f = self.fleet();
        let out = subscribe::approve(&f, &self.dirs.fleet(), host, raw)?;
        self.reload(&mut f)?;
        let allow = out
            .allow
            .as_ref()
            .map(|a| a.join(" "))
            .unwrap_or_else(|| "(none)".into());
        let mut ignore = Vec::new();
        self.journal(
            &mut ignore,
            Line::new("approve", host, "", format!("allow:{allow}")),
        );
        Ok(out)
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

#[cfg(test)]
#[path = "home_tests.rs"]
mod tests;
