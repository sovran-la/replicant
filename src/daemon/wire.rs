//! What crosses a socket: leaf daemon → home daemon over mTLS, and CLI →
//! local daemon over the Unix socket.
//!
//! Everything is versioned at the envelope. A daemon refuses a version it
//! does not know rather than guessing at it, as the agents crate does: a
//! half-understood subscription request is worse than a refused one.

use super::fleet::FleetState;
use crate::baseline::Baseline;
use crate::marker::{Direction, Marker};
use crate::state::UnitState;
use crate::units::UnitKind;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 2: per-host certificates. Home derives the caller from its certificate,
/// and a leaf collects its own with [`HomeRequest::CollectCert`].
/// 3: apply ([`HomeRequest::BeginPush`] and the rest of `sync`'s writes).
/// 4: [`HomeSnapshot::rolled_back`] (review A2), and `--accept-rollback`
/// (and `rollback_accepted` on [`HomeRequest::Agree`], re-review R2).
/// 5: the native data plane: [`HomeRequest::Manifest`], data connections
/// on the same listener, [`SubscriptionsView::streams`].
pub const WIRE_VERSION: u32 = 5;

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
        /// The leaf's own open markers, for the fleet view.
        #[serde(default)]
        markers: Vec<Marker>,
        /// For a real `sync`: whether it finished. Absent otherwise.
        #[serde(default)]
        sync_ok: Option<bool>,
    },
    FleetState,
    /// Apply, before a push's transfer: home is the target, so home writes
    /// the in-progress marker (or keeps this host's own, resuming) and opens
    /// the push's run, whose backup directory the receiver renames what it
    /// replaces into. Requires the caller to hold the head of the queue.
    BeginPush {
        lease: u64,
        rel: String,
        source_state: UnitState,
        /// The unit is a single file.
        #[serde(default)]
        is_file: bool,
    },
    /// Apply, after a push's transfer finished: home's state of the unit now,
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
    /// Agreeing settles a rollback of this host's push of `rel`, but one
    /// without a baseline only when `rollback_accepted` (re-review R2).
    Agree {
        lease: u64,
        rel: String,
        baseline: Option<Baseline>,
        /// The host passed `--accept-rollback` naming `rel`.
        #[serde(default)]
        rollback_accepted: bool,
    },
    /// Apply: the unit is done. Home clears its marker (a push) and
    /// journals `sync <host> <rel> <detail>`.
    Settle {
        lease: u64,
        rel: String,
        direction: Option<Direction>,
        detail: String,
        /// What the transfer reported writing (a push's manifest keeps it).
        #[serde(default)]
        changes: Vec<String>,
    },
    /// Apply: a unit's transfer failed or was stopped. Journaled; every marker
    /// stays. No lease needed: it is evidence, also after the lease is lost.
    Interrupted {
        rel: String,
        direction: Direction,
        why: String,
        /// What the transfer reported writing before it stopped.
        #[serde(default)]
        changes: Vec<String>,
    },
    /// Apply: a journal line about this host that is not a transfer:
    /// [`NOTE_EVENTS`] only.
    Note {
        event: String,
        rel: String,
        detail: String,
    },
    /// Home's manifest of one unit, walked with the unit's excludes: the
    /// root's (as home serves them) plus what only the leaf can say, every
    /// one of which can only leave more out. Requires the head of the queue
    /// (a dry run's entry will do: it reads).
    Manifest {
        lease: u64,
        rel: String,
        kind: UnitKind,
        is_file: bool,
        /// The plan's repos inside the unit (a loose unit leaves them out).
        union: Vec<String>,
        /// Both sides' worktree checkouts inside the unit.
        worktrees: Vec<String>,
        /// Both sides' git-ignored paths, unit-relative (a directory ends
        /// in `/`).
        ignored: Vec<String>,
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
    /// [`HomeRequest::Manifest`]'s answer.
    Manifest {
        manifest: crate::native::Manifest,
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

#[path = "wire_views.rs"]
mod views;
pub use views::*;

// ---------------------------------------------------------------- CLI → daemon

#[path = "wire_local.rs"]
mod local;
pub use local::*;
