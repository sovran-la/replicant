//! fleet.toml: home's whole world.
//!
//! Parsed with `toml` into raw structs, then resolved and validated into a
//! [`Fleet`]. Writes go through [`super::edit`], never through this module, so
//! hand-written comments survive. See PLAN.md, "Config lives on home".

use crate::paths;
use crate::{Error, Result};
use raw::RawFleet;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A leaf that has not talked to home for this long is stale.
pub const DEFAULT_STALE_AFTER_HOURS: u64 = 72;

/// How long a queue entry lives without a heartbeat.
pub const DEFAULT_LEASE_SECS: u64 = 30;
/// An in-progress marker older than this is reported as interrupted
/// rather than possibly still running (`[daemon] marker_alert_mins`).
pub const DEFAULT_MARKER_ALERT_MINS: u64 = 60;
/// How long home keeps a push's backup (`[daemon] backup_days`).
pub const DEFAULT_BACKUP_DAYS: u64 = 14;
/// Data connections per unit home accepts (`[daemon] streams`); a leaf's
/// `[home] streams` can only lower it.
pub const DEFAULT_STREAMS: u32 = 4;

/// A directory home serves, with its excludes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    pub name: String,
    /// The root relative to home's own home dir: `work`, `.claude`.
    pub rel: String,
    pub path: PathBuf,
    pub exclude: Vec<String>,
}

/// An enrolled host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEntry {
    pub home_dir: PathBuf,
    /// The cap, as normalized relative paths. `None` is "anything home serves".
    pub allow: Option<Vec<String>>,
    /// The pinned fingerprint of the host's client certificate
    /// (`sha256:<hex>` of its DER), written by `approve`. A request over that
    /// certificate *is* this host; `None` admits no certificate as it.
    pub cert: Option<String>,
}

/// What one host is subscribed to.
///
/// `except` carves a sub-path out of a broader subscription: subscribed to
/// `work`, then `unsubscribe ~/work/rust`.
///
/// `unsubscribed` lists subscriptions `unsubscribe` removed, until restored.
/// With `except` it tells a leaf a path was given up on purpose, the only
/// case it may remove its copy (review F5); a hand edit to `paths` is not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Subscription {
    pub paths: Vec<String>,
    pub except: Vec<String>,
    pub unsubscribed: Vec<String>,
}

/// A host that asked and has not been approved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub home_dir: PathBuf,
    pub paths: Vec<String>,
    pub requested_at: u64,
}

/// fleet.toml, resolved.
#[derive(Debug, Clone)]
pub struct Fleet {
    /// What leaves call home. `[daemon] name`, else this machine's hostname.
    pub name: String,
    pub listen: Option<String>,
    /// Home's own home dir. `[daemon] home_dir`, else `$HOME`.
    pub home_dir: PathBuf,
    pub stale_after: Duration,
    pub lease: Duration,
    pub marker_alert: Duration,
    pub backup_keep: Duration,
    /// Data connections per unit: `[daemon] streams`, at least 1.
    pub streams: u32,
    /// Sorted by `rel`.
    pub roots: Vec<Root>,
    pub hosts: BTreeMap<String, HostEntry>,
    pub subscriptions: BTreeMap<String, Subscription>,
    pub pending: BTreeMap<String, Pending>,
}

impl Fleet {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("could not read {}: {e}", path.display())))?;
        Self::parse(&text, path)
    }

    pub fn parse(text: &str, path: &Path) -> Result<Self> {
        let raw: RawFleet = toml::from_str(text)
            .map_err(|e| Error::Config(format!("{} is not valid: {e}", path.display())))?;
        let bad = |m: String| Error::Config(format!("{}: {m}", path.display()));

        let home_dir = match raw.daemon.home_dir {
            Some(h) => absolute(&h).map_err(bad)?,
            None => super::user_home()?,
        };
        let name = match raw.daemon.name {
            Some(n) => super::short_hostname(&n)
                .ok_or_else(|| bad(format!("[daemon] name = {n:?} is not a host name")))?,
            None => super::hostname().ok_or_else(|| {
                bad("this machine has no usable hostname; set [daemon] name".into())
            })?,
        };

        let mut roots = Vec::new();
        for (rname, r) in raw.roots {
            let rpath = absolute(&r.path).map_err(|m| bad(format!("[roots.{rname}] {m}")))?;
            let rel = rpath
                .strip_prefix(&home_dir)
                .ok()
                .and_then(|p| p.to_str())
                .filter(|p| !p.is_empty())
                .ok_or_else(|| {
                    bad(format!(
                        "[roots.{rname}] path {} is not inside home's home dir {}",
                        rpath.display(),
                        home_dir.display()
                    ))
                })?
                .to_string();
            for pat in &r.exclude {
                if pat.starts_with('/') || pat.is_empty() {
                    return Err(bad(format!(
                        "[roots.{rname}] exclude {pat:?}: anchored and empty patterns are not \
                         supported; write it without the leading `/` (a pattern with a `/` is \
                         relative to the root, e.g. \"agents/state\")"
                    )));
                }
                crate::exclude::check(pat)
                    .map_err(|why| bad(format!("[roots.{rname}] exclude {pat:?}: {why}")))?;
            }
            roots.push(Root {
                name: rname,
                rel,
                path: rpath,
                exclude: r.exclude,
            });
        }
        roots.sort_by(|a, b| a.rel.cmp(&b.rel));
        for pair in roots.windows(2) {
            if paths::is_within(&pair[1].rel, &pair[0].rel) {
                return Err(bad(format!(
                    "roots {} and {} overlap; a path must belong to exactly one root",
                    pair[0].name, pair[1].name
                )));
            }
        }

        let mut hosts = BTreeMap::new();
        let mut pinned: BTreeMap<String, String> = BTreeMap::new();
        for (h, e) in raw.hosts {
            let hd = absolute(&e.home_dir).map_err(|m| bad(format!("[hosts.{h}] {m}")))?;
            let allow = e
                .allow
                .map(|list| check_rels(&list).map_err(|m| bad(format!("[hosts.{h}] allow: {m}"))))
                .transpose()?;
            if let Some(c) = &e.cert {
                check_fingerprint(c).map_err(|m| bad(format!("[hosts.{h}] cert: {m}")))?;
                if let Some(other) = pinned.insert(c.clone(), h.clone()) {
                    return Err(bad(format!(
                        "[hosts.{other}] and [hosts.{h}] pin the same cert; one certificate is \
                         one host"
                    )));
                }
            }
            hosts.insert(
                h,
                HostEntry {
                    home_dir: hd,
                    allow,
                    cert: e.cert,
                },
            );
        }

        let mut subscriptions = BTreeMap::new();
        for (h, s) in raw.subscriptions {
            let ctx = |m| bad(format!("[subscriptions.{h}] {m}"));
            subscriptions.insert(
                h.clone(),
                Subscription {
                    paths: check_rels(&s.paths).map_err(ctx)?,
                    except: check_rels(&s.except).map_err(ctx)?,
                    unsubscribed: check_rels(&s.unsubscribed).map_err(ctx)?,
                },
            );
        }

        let mut pending = BTreeMap::new();
        for (h, p) in raw.pending {
            let hd = absolute(&p.home_dir).map_err(|m| bad(format!("[pending.{h}] {m}")))?;
            let ps = check_rels(&p.paths).map_err(|m| bad(format!("[pending.{h}] {m}")))?;
            pending.insert(
                h,
                Pending {
                    home_dir: hd,
                    paths: ps,
                    requested_at: p.requested_at,
                },
            );
        }

        Ok(Self {
            name,
            listen: raw.daemon.listen,
            home_dir,
            stale_after: Duration::from_secs(
                raw.daemon
                    .stale_after_hours
                    .unwrap_or(DEFAULT_STALE_AFTER_HOURS)
                    * 3600,
            ),
            lease: Duration::from_secs(raw.daemon.lease_secs.unwrap_or(DEFAULT_LEASE_SECS).max(1)),
            backup_keep: Duration::from_secs(
                raw.daemon.backup_days.unwrap_or(DEFAULT_BACKUP_DAYS) * 86_400,
            ),
            marker_alert: Duration::from_secs(
                raw.daemon
                    .marker_alert_mins
                    .unwrap_or(DEFAULT_MARKER_ALERT_MINS)
                    * 60,
            ),
            streams: raw.daemon.streams.unwrap_or(DEFAULT_STREAMS).max(1),
            roots,
            hosts,
            subscriptions,
            pending,
        })
    }

    /// The root a normalized relative path belongs to.
    pub fn root_for(&self, rel: &str) -> Option<&Root> {
        self.roots.iter().find(|r| paths::is_within(rel, &r.rel))
    }

    /// The roots' relative names, for "anything not under a root → refused".
    pub fn root_rels(&self) -> Vec<String> {
        self.roots.iter().map(|r| r.rel.clone()).collect()
    }

    /// Where a normalized relative path lives on home.
    pub fn home_path(&self, rel: &str) -> PathBuf {
        self.home_dir.join(rel)
    }

    /// The enrolled host whose pinned certificate has fingerprint `fp`.
    pub fn host_for_cert(&self, fp: &str) -> Option<&str> {
        self.hosts
            .iter()
            .find(|(_, e)| e.cert.as_deref() == Some(fp))
            .map(|(h, _)| h.as_str())
    }

    /// One host's subscription, or the empty one.
    pub fn subscription(&self, host: &str) -> Subscription {
        self.subscriptions.get(host).cloned().unwrap_or_default()
    }
}

fn absolute(raw: &str) -> std::result::Result<PathBuf, String> {
    let p = PathBuf::from(raw.trim_end_matches('/'));
    if !p.is_absolute() || paths::has_dotdot(raw) {
        return Err(format!("{raw:?} must be an absolute path with no `..`"));
    }
    Ok(p)
}

/// `sha256:` and 64 lowercase hex digits, as `approve` writes it.
fn check_fingerprint(raw: &str) -> std::result::Result<(), String> {
    let hex = raw.strip_prefix("sha256:").unwrap_or("");
    let ok = hex.len() == 64
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "{raw:?} is not a fingerprint; `approve` writes \"sha256:<64 lowercase hex>\""
        ))
    }
}

fn check_rels(list: &[String]) -> std::result::Result<Vec<String>, String> {
    list.iter()
        .map(|p| {
            paths::check_rel(p).map_err(|why| format!("{p:?} {why}"))?;
            Ok(p.clone())
        })
        .collect()
}

#[path = "fleet_raw.rs"]
mod raw;

#[cfg(test)]
#[path = "fleet_tests.rs"]
mod tests;
