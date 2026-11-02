//! `fleet_state`: home's whole world as one query. PLAN.md, "Fleet view".
//!
//! Every renderer is thin over this: `replicant status --fleet` today, an MCP
//! panel or dashboard pane later. What a leaf decided is only known as of its
//! last report, so each row carries when that was; **staleness** (no contact
//! for longer than `stale_after_hours`) is what makes a quietly diverging leaf
//! visible from anywhere.

use super::queue::QueueView;
use super::wire::ReportedUnit;
use crate::Result;
use crate::config::Fleet;
use crate::marker::Marker;
use crate::record::{RECORD_VERSION, Record, Store};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// What home last heard from a host: `state/hosts/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    pub version: u32,
    /// The host name (the record's key).
    pub rel: String,
    pub last_contact: u64,
    pub report: Option<LastReport>,
    /// The host's last real `sync`, whatever it reported since.
    #[serde(default)]
    pub last_sync: Option<LastSync>,
    /// The host's own open markers (interrupted pulls) as of its last
    /// report.
    #[serde(default)]
    pub markers: Vec<Marker>,
}

/// How a host's last `sync` ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastSync {
    pub at: u64,
    /// Every unit applied, or nothing to apply.
    pub ok: bool,
    pub summary: String,
}

impl Record for Contact {
    fn rel(&self) -> &str {
        &self.rel
    }
    fn version(&self) -> u32 {
        self.version
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastReport {
    pub at: u64,
    pub event: String,
    pub units: Vec<ReportedUnit>,
}

pub fn contacts(state_dir: &Path) -> Store {
    Store::at(PathBuf::from(state_dir).join("hosts"))
}

/// Record that `host` was heard from now, keeping its last report.
pub fn touch(store: &Store, host: &str, report: Option<LastReport>) -> Result<()> {
    let prior: Option<Contact> = store.get(host)?;
    let (last_sync, markers) = prior
        .as_ref()
        .map(|p| (p.last_sync.clone(), p.markers.clone()))
        .unwrap_or_default();
    let c = Contact {
        version: RECORD_VERSION,
        rel: host.into(),
        last_contact: crate::now_secs(),
        report: report.or(prior.and_then(|p| p.report)),
        last_sync,
        markers,
    };
    store.put(&c)
}

/// What a host's report says: its decisions, its open markers, and, for
/// a real `sync`, how that ended.
pub fn reported(
    store: &Store,
    host: &str,
    report: LastReport,
    markers: Vec<Marker>,
    sync: Option<LastSync>,
) -> Result<()> {
    let prior: Option<Contact> = store.get(host)?;
    let c = Contact {
        version: RECORD_VERSION,
        rel: host.into(),
        last_contact: crate::now_secs(),
        report: Some(report),
        last_sync: sync.or(prior.and_then(|p| p.last_sync)),
        markers,
    };
    store.put(&c)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingView {
    pub host: String,
    pub home_dir: PathBuf,
    pub paths: Vec<String>,
    pub requested_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRow {
    pub name: String,
    pub home_dir: PathBuf,
    pub cap: Option<Vec<String>>,
    pub paths: Vec<String>,
    pub except: Vec<String>,
    pub last_contact: Option<u64>,
    pub stale: bool,
    pub report: Option<LastReport>,
    /// 0 is the head.
    pub queue_position: Option<usize>,
    #[serde(default)]
    pub last_sync: Option<LastSync>,
    /// The host's own open markers as of its last report.
    #[serde(default)]
    pub markers: Vec<Marker>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetState {
    pub home: String,
    pub now: u64,
    pub stale_after_secs: u64,
    pub hosts: Vec<HostRow>,
    pub pending: Vec<PendingView>,
    pub queue: Vec<QueueView>,
    /// Home's in-progress markers: each one quarantines its path.
    pub markers: Vec<Marker>,
}

pub fn pending_views(fleet: &Fleet) -> Vec<PendingView> {
    fleet
        .pending
        .iter()
        .map(|(h, p)| PendingView {
            host: h.clone(),
            home_dir: p.home_dir.clone(),
            paths: p.paths.clone(),
            requested_at: p.requested_at,
        })
        .collect()
}

/// Assemble the state. `now` is a parameter so staleness is testable.
pub fn build(
    fleet: &Fleet,
    contacts: &Store,
    queue: Vec<QueueView>,
    markers: Vec<Marker>,
    now: u64,
) -> Result<FleetState> {
    let stale_after = fleet.stale_after.as_secs();
    let mut hosts = Vec::new();
    for (name, entry) in &fleet.hosts {
        let c: Option<Contact> = contacts.get(name)?;
        let last = c.as_ref().map(|c| c.last_contact);
        let sub = fleet.subscription(name);
        hosts.push(HostRow {
            name: name.clone(),
            home_dir: entry.home_dir.clone(),
            cap: entry.allow.clone(),
            paths: sub.paths,
            except: sub.except,
            last_contact: last,
            stale: last.is_none_or(|t| now.saturating_sub(t) > stale_after),
            queue_position: queue.iter().position(|q| q.host == *name),
            last_sync: c.as_ref().and_then(|c| c.last_sync.clone()),
            markers: c.as_ref().map(|c| c.markers.clone()).unwrap_or_default(),
            report: c.and_then(|c| c.report),
        });
    }
    Ok(FleetState {
        home: fleet.name.clone(),
        now,
        stale_after_secs: stale_after,
        hosts,
        pending: pending_views(fleet),
        queue,
        markers,
    })
}

#[cfg(test)]
#[path = "fleet_tests.rs"]
mod tests;
