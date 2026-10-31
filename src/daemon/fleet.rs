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
    let c = Contact {
        version: RECORD_VERSION,
        rel: host.into(),
        last_contact: crate::now_secs(),
        report: report.or(prior.and_then(|p| p.report)),
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
            report: c.and_then(|c| c.report),
            queue_position: queue.iter().position(|q| q.host == *name),
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
mod tests {
    use super::*;
    use crate::daemon::queue::EntryKind;
    use crate::decide::Decision;

    const FLEET: &str = "[daemon]\nname=\"tc\"\nhome_dir=\"/h\"\nstale_after_hours=1\n\
        [roots.work]\npath=\"/h/work\"\n\
        [hosts.fresh]\nhome_dir=\"/u\"\n[hosts.stale]\nhome_dir=\"/u\"\nallow=[\"work/x\"]\n\
        [hosts.never]\nhome_dir=\"/u\"\n\
        [subscriptions.stale]\npaths=[\"work/x\"]\n\
        [pending.newbie]\nhome_dir=\"/u\"\npaths=[\"work\"]\nrequested_at=5\n";

    #[test]
    fn one_row_per_host_with_staleness_reports_and_queue() {
        let tmp = tempfile::tempdir().unwrap();
        let fleet = Fleet::parse(FLEET, Path::new("fleet.toml")).unwrap();
        let store = contacts(tmp.path());
        let now = crate::now_secs();
        store
            .put(&Contact {
                version: RECORD_VERSION,
                rel: "fresh".into(),
                last_contact: now - 60,
                report: None,
            })
            .unwrap();
        let report = LastReport {
            at: now - 9 * 86_400,
            event: "dry-run".into(),
            units: vec![ReportedUnit {
                rel: "work/x".into(),
                decision: Decision::Push,
            }],
        };
        store
            .put(&Contact {
                version: RECORD_VERSION,
                rel: "stale".into(),
                last_contact: now - 9 * 86_400,
                report: Some(report.clone()),
            })
            .unwrap();
        let q = vec![QueueView {
            id: 1,
            host: "stale".into(),
            kind: EntryKind::DryRun,
            paths: vec![],
            enqueued_at: now,
            started_at: Some(now),
        }];
        let s = build(&fleet, &store, q, vec![], now).unwrap();
        assert_eq!(s.home, "tc");
        let names: Vec<_> = s.hosts.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, ["fresh", "never", "stale"]);
        let row = |n: &str| s.hosts.iter().find(|h| h.name == n).unwrap();
        assert!(!row("fresh").stale);
        assert!(row("stale").stale, "nine days without contact");
        assert!(row("never").stale && row("never").last_contact.is_none());
        assert_eq!(row("stale").report, Some(report));
        assert_eq!(row("stale").queue_position, Some(0));
        assert_eq!(row("stale").cap, Some(vec!["work/x".to_string()]));
        assert_eq!(s.pending.len(), 1);
        assert_eq!(s.pending[0].host, "newbie");
    }

    #[test]
    fn touch_keeps_the_last_report() {
        let tmp = tempfile::tempdir().unwrap();
        let store = contacts(tmp.path());
        let r = LastReport {
            at: 1,
            event: "status".into(),
            units: vec![],
        };
        touch(&store, "h", Some(r.clone())).unwrap();
        touch(&store, "h", None).unwrap();
        let c: Contact = store.get("h").unwrap().unwrap();
        assert_eq!(c.report, Some(r));
        assert!(c.last_contact > 0);
    }
}
