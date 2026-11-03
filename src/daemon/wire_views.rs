//! What home tells a leaf about its subscriptions and its side of them.

use crate::decide::{Decision, RolledBackPush, UnservedReason};
use crate::guard::Refusal;
use crate::marker::Marker;
use crate::units::{SideUnit, SubSpec};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::Enrollment;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionsView {
    pub home: String,
    pub enrollment: Enrollment,
    /// In-bounds subscriptions, with their root excludes and carve-outs.
    pub specs: Vec<SubSpec>,
    pub cap: Option<Vec<String>>,
    /// Subscriptions home will not serve because no root covers them:
    /// home's config is broken, so the leaf blocks.
    pub refused: Vec<Refusal>,
    /// Subscriptions home will not serve by policy: outside a cap narrowed
    /// since, or wholly inside a root exclude. Shown, never blocking (review
    /// F5). Absent from an older home: none.
    #[serde(default)]
    pub orphaned: Vec<Refusal>,
    /// For a pending host, what it asked for.
    pub pending_paths: Vec<String>,
    pub lease_secs: u64,
    /// A marker older than this is reported as interrupted (fleet.toml
    /// `[daemon] marker_alert_mins`). Absent from an older home: an hour.
    #[serde(default = "default_marker_alert")]
    pub marker_alert_secs: u64,
    /// Data connections home accepts per unit (`[daemon] streams`).
    #[serde(default = "default_streams")]
    pub streams: u32,
}

fn default_streams() -> u32 {
    crate::config::fleet::DEFAULT_STREAMS
}

fn default_marker_alert() -> u64 {
    crate::config::fleet::DEFAULT_MARKER_ALERT_MINS * 60
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeSnapshot {
    /// Home's home dir: a unit `rel` lives at `home_dir/rel` on home.
    pub home_dir: PathBuf,
    pub specs: Vec<SubSpec>,
    pub units: Vec<SideUnit>,
    pub repos: Vec<String>,
    pub worktrees: Vec<String>,
    /// Home's markers covering any of this host's subscriptions.
    pub markers: Vec<Marker>,
    /// Subscriptions whose base does not exist on home (its root does). The
    /// leaf, which holds the baselines, decides whether that is a first
    /// sync or an error (review F4). Absent from an older home: none.
    #[serde(default)]
    pub absent: Vec<String>,
    /// The leaf's baselines home no longer serves, and why. A baseline out
    /// of bounds and missing here is [`UnservedReason::NotServed`]. Absent
    /// from an older home: none.
    #[serde(default)]
    pub unserved: Vec<Unserved>,
    /// This host's pushes home rolled back that it has not settled (review
    /// A2): the newest per unit. Such a unit holds rather than pulls.
    #[serde(default)]
    pub rolled_back: Vec<RolledBackPush>,
}

/// A path a leaf has a baseline for and home no longer serves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unserved {
    /// The baseline's path, with the excludes and carve-outs home would
    /// apply to it if it still served it (none when no root covers it), so
    /// the leaf hashes its copy the way the baseline was hashed.
    pub spec: SubSpec,
    pub reason: UnservedReason,
    /// What home can say about it: for a missing root, which subscription
    /// lost it and which roots fleet.toml has.
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedUnit {
    pub rel: String,
    pub decision: Decision,
}
