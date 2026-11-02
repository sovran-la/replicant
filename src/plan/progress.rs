//! What a `sync` tells whoever waits on it while it applies: each unit as
//! it starts and as it ends, in plan order. PLAN.md, "Collect, then apply".
//!
//! Only units that do something are announced (a transfer, an adoption, a
//! forgotten baseline); `index` and `total` count those. A unit that does not
//! finish is announced done with why, and is the last: the sync stops there.
//! Planning, dry runs and `status` announce nothing.

use crate::marker::Direction;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ProgressEvent {
    UnitStarted {
        rel: String,
        /// As [`super::apply::AppliedUnit::action`]: `pull`, `push`,
        /// `remove`, `delete on home`, `delete here`, `adopt`, `forget`.
        action: String,
        /// `None` for an adoption or a forgotten baseline: no rsync.
        direction: Option<Direction>,
        /// 1-based, of `total`.
        index: usize,
        total: usize,
    },
    UnitDone {
        rel: String,
        action: String,
        direction: Option<Direction>,
        files_changed: usize,
        deleted: usize,
        elapsed_ms: u64,
        /// A push's id on home: its backup, for `replicant rollback`.
        push: Option<String>,
        outcome: UnitOutcome,
    },
}

/// How one unit ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UnitOutcome {
    Applied,
    /// Its rsync did not finish (failed, timed out, or stopped: the lease
    /// lost). Its marker stays; the next `sync` resumes it.
    Interrupted {
        why: String,
    },
    /// A step around its rsync failed, or it changed since the plan.
    Failed {
        why: String,
    },
}

/// Where a sync sends its events.
pub struct Progress<'a> {
    emit: &'a dyn Fn(ProgressEvent),
}

fn ignore(_: ProgressEvent) {}

impl<'a> Progress<'a> {
    pub fn new(emit: &'a dyn Fn(ProgressEvent)) -> Self {
        Self { emit }
    }

    /// Nobody is listening.
    pub fn silent() -> Progress<'static> {
        Progress { emit: &ignore }
    }

    pub fn emit(&self, e: ProgressEvent) {
        (self.emit)(e)
    }
}
