//! What crosses a socket: leaf daemon → home daemon over mTLS, and CLI →
//! local daemon over the Unix socket.
//!
//! Everything is versioned at the envelope. A daemon refuses a version it
//! does not know rather than guessing at it, as the agents crate does: a
//! half-understood subscription request is worse than a refused one.

use super::fleet::FleetState;
use crate::baseline::Baseline;
use crate::decide::{Decision, UnservedReason};
use crate::guard::Refusal;
use crate::marker::{Direction, Marker};
use crate::state::UnitState;
use crate::units::UnitKind;
use crate::units::{SideUnit, SubSpec};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 2: per-host certificates. Home derives the caller from its certificate,
/// and a leaf collects its own with [`HomeRequest::CollectCert`].
/// 3: apply ([`HomeRequest::BeginPush`] and the rest of `sync`'s writes).
pub const WIRE_VERSION: u32 = 3;

// ---------------------------------------------------------------- leaf → home

/// One request from a leaf's daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeCall {
    pub version: u32,
    /// The leaf's fleet name. Never trusted for identity: home derives the
    /// host from the peer certificate and refuses a request that names
    /// another. Over the bootstrap bundle it names the host asking to be
    /// enrolled, or collecting its certificate.
    pub host: String,
    /// The leaf's `$HOME`, for path normalization and pending enrollment.
    pub home_dir: PathBuf,
    pub request: HomeRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum HomeRequest {
    Subscribe {
        paths: Vec<String>,
    },
    Unsubscribe {
        paths: Vec<String>,
    },
    Subscriptions,
    Acquire {
        no_wait: bool,
        /// A real `sync` rather than a dry run (shown in the queue). Absent
        /// from an older leaf: a dry run.
        #[serde(default)]
        apply: bool,
    },
    Heartbeat {
        lease: u64,
    },
    Release {
        lease: u64,
    },
    /// Home's side of every subscribed unit.
    State {
        leaf_repos: Vec<String>,
        case_check: bool,
        /// Every path the leaf holds a baseline for, so home can say why it
        /// no longer serves any of them (review F5). Absent from an older
        /// leaf: none.
        #[serde(default)]
        baselines: Vec<String>,
    },
    /// What the leaf decided, for the fleet view and the journal.
    Report {
        event: String,
        units: Vec<ReportedUnit>,
        summary: String,
    },
    FleetState,
    /// Apply, before a push's real rsync: home is the target, so home writes
    /// the in-progress marker (or keeps this host's own, resuming).
    /// Requires the caller to hold the head of the queue.
    BeginPush {
        lease: u64,
        rel: String,
        source_state: UnitState,
        /// The files (unit-relative) the push will replace or delete, from
        /// its dry run just before: home copies them to the push's backup
        /// before it answers.
        #[serde(default)]
        backup: Vec<String>,
        /// The unit is a single file.
        #[serde(default)]
        is_file: bool,
    },
    /// Apply, after a push's rsync exited 0: home's state of the unit now,
    /// hashed as the plan hashed it. `union` is the plan's repos within the
    /// unit.
    Rehash {
        lease: u64,
        rel: String,
        kind: UnitKind,
        union: Vec<String>,
    },
    /// Apply: both sides agree on `rel`. Home records `baseline` as its copy
    /// for this host, or drops its copy when `None` (the unit was deleted).
    Agree {
        lease: u64,
        rel: String,
        baseline: Option<Baseline>,
    },
    /// Apply: the unit is done. Home clears its marker (a push) and
    /// journals `sync <host> <rel> <detail>`.
    Settle {
        lease: u64,
        rel: String,
        direction: Option<Direction>,
        detail: String,
        /// What rsync reported writing (a push's manifest keeps it).
        #[serde(default)]
        changes: Vec<String>,
    },
    /// Apply: a unit's rsync failed or was stopped. Journaled; every marker
    /// stays. No lease needed: it is evidence, also after the lease is lost.
    Interrupted {
        rel: String,
        direction: Direction,
        why: String,
        /// What rsync reported writing before it stopped.
        #[serde(default)]
        changes: Vec<String>,
    },
    /// Apply: a journal line about this host that is not an rsync:
    /// [`NOTE_EVENTS`] only.
    Note {
        event: String,
        rel: String,
        detail: String,
    },
    /// Over the bootstrap bundle: the certificate `approve` issued this
    /// host, if one is waiting. One-shot: home deletes its copy as it
    /// answers.
    CollectCert,
}

/// Home's answer, plus any journal lines it wrote about the caller so the
/// leaf can keep its own copy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeAnswer {
    pub result: Result<HomeReply, String>,
    #[serde(default)]
    pub journal: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum HomeReply {
    Subscribed(SubscribeOutcome),
    Unsubscribed(UnsubscribeOutcome),
    Subscriptions(SubscriptionsView),
    Lease {
        lease: u64,
        lease_secs: u64,
    },
    Done,
    State(HomeSnapshot),
    Fleet(FleetState),
    Collected(Collected),
    /// [`HomeRequest::Rehash`]'s answer.
    Rehashed {
        state: UnitState,
    },
    /// [`HomeRequest::BeginPush`]'s answer: the push's id on home, which
    /// names its backup.
    Begun {
        push: String,
    },
}

/// The events a leaf may journal with [`HomeRequest::Note`]: a deletion it
/// was told to carry out, and a shared state adopted as the first baseline.
pub const NOTE_EVENTS: &[&str] = &["confirm-delete", "adopt"];

/// What [`HomeRequest::CollectCert`] found for the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Collected {
    /// The host's identity bundle (certificate, key, CA). Home no longer
    /// has a copy.
    Issued { bundle: String },
    /// Waiting for `replicant approve` on home.
    Pending { home: String, paths: Vec<String> },
    /// Home has no record of the host: it has not subscribed.
    Unknown { home: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscribeOutcome {
    /// false: recorded as pending, waiting for `replicant approve` on home.
    pub enrolled: bool,
    pub home: String,
    /// The normalized paths of this request, echoed back.
    pub requested: Vec<String>,
    /// The host's whole subscription (or pending request) afterwards.
    pub paths: Vec<String>,
    /// Paths dropped because a broader one covers them.
    pub absorbed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsubscribeOutcome {
    /// Whole subscriptions removed.
    pub removed: Vec<String>,
    /// Sub-paths carved out of a broader subscription.
    pub carved: Vec<String>,
    pub paths: Vec<String>,
    pub except: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Enrollment {
    Enrolled,
    Pending,
    Unknown,
}

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

// ---------------------------------------------------------------- CLI → daemon

#[path = "wire_local.rs"]
mod local;
pub use local::*;
