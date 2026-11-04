//! What a `sync` tells whoever waits on it while it applies: each unit as
//! it starts, how far its transfer has got, and as it ends, in plan order. PLAN.md, "Collect, then apply".
//!
//! Only units that do something are announced (a transfer, an adoption, a
//! forgotten baseline); `index` and `total` count those. A unit that does not
//! finish is announced done with why, and is the last: the sync stops there.
//! Planning, dry runs and `status` announce nothing.

use crate::abort::Abort;
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
        /// `None` for an adoption or a forgotten baseline: no transfer.
        direction: Option<Direction>,
        /// 1-based, of `total`.
        index: usize,
        total: usize,
    },
    /// A unit's transfer so far, at most every 250 ms while it sends:
    /// files and bytes done of the unit's totals. The start's fields again,
    /// so a renderer can redraw the unit's line from this alone.
    UnitProgress {
        rel: String,
        action: String,
        index: usize,
        total: usize,
        files_done: u64,
        files_total: u64,
        bytes_done: u64,
        bytes_total: u64,
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
    /// Its transfer did not finish (failed, a dead connection, or stopped: the lease
    /// lost, the CLI gone). Its marker stays; the next `sync` resumes it.
    Interrupted {
        why: String,
    },
    /// A step around its transfer failed, or it changed since the plan.
    Failed {
        why: String,
    },
}

/// Where a sync sends its events, and the flag that stops it: raised, the
/// running transfer is stopped and the unit left interrupted, and no further
/// unit begins.
pub struct Progress<'a> {
    emit: &'a (dyn Fn(ProgressEvent) + Sync),
    abort: Abort,
}

fn ignore(_: ProgressEvent) {}

impl<'a> Progress<'a> {
    /// `emit` is called from the thread that runs the sync, and for a
    /// unit's counters from its transfer's monitor, never two at once.
    pub fn new(emit: &'a (dyn Fn(ProgressEvent) + Sync), abort: Abort) -> Self {
        Self { emit, abort }
    }

    /// Nobody is listening, and nothing but a lost lease stops it.
    pub fn silent() -> Progress<'static> {
        Progress {
            emit: &ignore,
            abort: Abort::default(),
        }
    }

    pub fn emit(&self, e: ProgressEvent) {
        (self.emit)(e)
    }

    pub fn abort(&self) -> &Abort {
        &self.abort
    }
}
