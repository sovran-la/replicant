//! CLI → local daemon, over the Unix socket. Same binary on both ends, same
//! [`super::WIRE_VERSION`] envelope.

use super::{SubscribeOutcome, UnsubscribeOutcome};
use crate::daemon::fleet::{FleetState, PendingView};
use crate::daemon::journal::{Filter, Line};
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
    Revoked(Revoked),
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
