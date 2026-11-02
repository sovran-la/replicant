//! CLI → local daemon, over the Unix socket. Same binary on both ends, same
//! [`super::WIRE_VERSION`] envelope.

use super::{SubscribeOutcome, UnsubscribeOutcome};
use crate::daemon::fleet::{FleetState, PendingView};
use crate::daemon::journal::{Filter, Line};
use crate::plan::apply::{SyncOpts, SyncReport};
use crate::plan::{LeafStatus, Plan};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
    /// `sync` without `--dry-run`: plan, and apply what may be applied.
    Sync {
        opts: SyncOpts,
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
    Revoke {
        host: String,
    },
    /// On home: undo one push, or (`None`) list the pushes.
    Rollback {
        push: Option<String>,
    },
}

pub type LocalAnswer = Result<LocalReply, String>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum LocalReply {
    Pong {
        role: String,
        name: String,
    },
    Subscribed(SubscribeOutcome),
    Unsubscribed(UnsubscribeOutcome),
    LeafStatus(Box<LeafStatus>),
    Fleet(FleetState),
    Plan(Box<Plan>),
    Synced(Box<SyncReport>),
    Log {
        lines: Vec<Line>,
    },
    Pending {
        hosts: Vec<PendingView>,
    },
    AllowPreview(AllowPreview),
    Approved(Approved),
    Reissued(Reissued),
    Revoked(Revoked),
    RolledBack(RolledBack),
    Pushes {
        pushes: Vec<crate::daemon::home::Push>,
    },
}

/// `rollback <push-id>`: what was put back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolledBack {
    pub id: String,
    pub unit: String,
    /// The leaf that pushed.
    pub host: String,
    /// Files restored from the backup.
    pub restored: usize,
    /// Paths the push had created, deleted.
    pub removed: usize,
    /// Files the push changed that were not backed up (it wrote more than
    /// its dry run said): left as pushed.
    pub unbacked: Vec<String>,
    /// Runs rsync never reported on; what they created stays.
    pub unreported: Vec<usize>,
    /// Where what the push had written was kept before it was undone.
    pub kept: PathBuf,
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

/// `host revoke <host>`: the host's pin removed, so its certificate is
/// unknown to home and refused; enrollment and subscriptions are kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revoked {
    pub host: String,
    /// The pin removed, if there was one.
    pub cert: Option<String>,
    /// An issued certificate was waiting uncollected, and is gone.
    pub uncollected: bool,
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
