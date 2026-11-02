//! What crosses a socket: leaf daemon → home daemon over mTLS, and CLI →
//! local daemon over the Unix socket.
//!
//! Everything is versioned at the envelope. A daemon refuses a version it
//! does not know rather than guessing at it, as the agents crate does: a
//! half-understood subscription request is worse than a refused one.

use super::fleet::{FleetState, PendingView};
use super::journal::{Filter, Line};
use crate::decide::{Decision, UnservedReason};
use crate::guard::Refusal;
use crate::marker::Marker;
use crate::plan::{LeafStatus, Plan};
use crate::units::{SideUnit, SubSpec};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const WIRE_VERSION: u32 = 1;

// ---------------------------------------------------------------- leaf → home

/// One request from a leaf's daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeCall {
    pub version: u32,
    /// The leaf's fleet name. Self-asserted: see [`super::certs`].
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
    Lease { lease: u64, lease_secs: u64 },
    Done,
    State(HomeSnapshot),
    Fleet(FleetState),
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalCall {
    pub version: u32,
    pub request: LocalRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum LocalRequest {
    Ping,
    Subscribe {
        home: String,
        address: Option<String>,
        rsync_host: Option<String>,
        credentials: Option<String>,
        paths: Vec<String>,
        as_local: Option<PathBuf>,
    },
    Unsubscribe {
        paths: Vec<String>,
    },
    Status,
    Fleet,
    DryRun {
        no_wait: bool,
    },
    Log {
        filter: Filter,
    },
    Pending,
    PreviewAllow {
        host: String,
        raw: Vec<String>,
    },
    Approve {
        host: String,
        allow: Vec<String>,
    },
    Reissue {
        host: String,
    },
}

pub type LocalAnswer = Result<LocalReply, String>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum LocalReply {
    Pong { role: String, name: String },
    Subscribed(SubscribeOutcome),
    Unsubscribed(UnsubscribeOutcome),
    LeafStatus(Box<LeafStatus>),
    Fleet(FleetState),
    Plan(Box<Plan>),
    Log { lines: Vec<Line> },
    Pending { hosts: Vec<PendingView> },
    AllowPreview(AllowPreview),
    Approved(Approved),
    Reissued(Reissued),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowPreview {
    pub host: String,
    /// The normalized cap, or `None` for no cap.
    pub allow: Option<Vec<String>>,
    /// Requested paths the cap admits.
    pub keep: Vec<String>,
    /// Requested paths the cap refuses; approving drops them.
    pub drop: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approved {
    pub host: String,
    pub allow: Option<Vec<String>>,
    pub subscribed: Vec<String>,
    pub dropped: Vec<String>,
    /// The pinned fingerprint of the certificate minted for the host.
    pub cert: String,
}

/// `approve <host>` for an enrolled host: a new certificate, pinned in place
/// of the old one, waiting for the host to collect it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reissued {
    pub host: String,
    pub cert: String,
    /// The pin it replaced, if there was one.
    pub replaced: Option<String>,
}
