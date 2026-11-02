//! fleet.toml as serde reads it, before [`super::Fleet::parse`] resolves
//! and validates it.

use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawFleet {
    #[serde(default)]
    pub(super) daemon: RawDaemon,
    #[serde(default)]
    pub(super) roots: BTreeMap<String, RawRoot>,
    #[serde(default)]
    pub(super) hosts: BTreeMap<String, RawHost>,
    #[serde(default)]
    pub(super) subscriptions: BTreeMap<String, RawSubscription>,
    #[serde(default)]
    pub(super) pending: BTreeMap<String, RawPending>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawDaemon {
    pub(super) listen: Option<String>,
    pub(super) name: Option<String>,
    pub(super) home_dir: Option<String>,
    pub(super) stale_after_hours: Option<u64>,
    pub(super) lease_secs: Option<u64>,
    pub(super) marker_alert_mins: Option<u64>,
    pub(super) backup_days: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawRoot {
    pub(super) path: String,
    #[serde(default)]
    pub(super) exclude: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawHost {
    pub(super) home_dir: String,
    pub(super) allow: Option<Vec<String>>,
    pub(super) cert: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawSubscription {
    #[serde(default)]
    pub(super) paths: Vec<String>,
    #[serde(default)]
    pub(super) except: Vec<String>,
    #[serde(default)]
    pub(super) unsubscribed: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawPending {
    pub(super) home_dir: String,
    #[serde(default)]
    pub(super) paths: Vec<String>,
    #[serde(default)]
    pub(super) requested_at: u64,
}
