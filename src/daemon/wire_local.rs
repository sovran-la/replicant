//! CLI → local daemon, over the Unix socket. Same binary on both ends; its
//! own version ([`LOCAL_WIRE_VERSION`]), so a change here does not make
//! home and its leaves upgrade together.
//!
//! One [`LocalCall`] goes in; [`LocalFrame`]s come back, one per line: any
//! number of [`LocalFrame::Progress`] (only a `sync` that applies sends
//! them), then exactly one answer, `Ok` or `Err`.

use super::{SubscribeOutcome, UnsubscribeOutcome};
use crate::daemon::fleet::{FleetState, PendingView};
use crate::daemon::journal::{Filter, Line};
use crate::plan::apply::{ProgressEvent, SyncOpts, SyncReport};
use crate::plan::{LeafStatus, Plan};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 4: the shared [`super::WIRE_VERSION`], before the CLI had its own.
/// 5: the answer is a stream of [`LocalFrame`]s (a sync's progress).
pub const LOCAL_WIRE_VERSION: u32 = 5;

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
        /// `--accept-rollback`, so the plan shown is the one `Sync` makes.
        #[serde(default)]
        accept_rollback: Vec<String>,
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

/// One line from the daemon. `Ok` and `Err` are spelled as a
/// [`LocalAnswer`] is (`{"Ok":…}`, `{"Err":"…"}`), so an older daemon's
/// refusal of this CLI's version still reads as the refusal it is.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LocalFrame {
    Progress(ProgressEvent),
    Ok(LocalReply),
    Err(String),
}

impl From<LocalAnswer> for LocalFrame {
    fn from(a: LocalAnswer) -> Self {
        match a {
            Ok(r) => LocalFrame::Ok(r),
            Err(e) => LocalFrame::Err(e),
        }
    }
}

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
