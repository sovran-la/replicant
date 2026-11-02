//! Home's FIFO queue: syncs and holds, with leases. PLAN.md, "The queue".
//!
//! One invariant: an entry proceeds only when every entry ahead of it is
//! completely done. Nothing interleaves on home.
//!
//! Every entry carries a lease, of the length it was granted at enqueue; the
//! fleet's policy is read once, there, and never again for that entry. The
//! holder of the head heartbeats; an entry whose lease runs out is **expired** — removed and reported, so the next
//! entry proceeds. Expiry releases the queue and nothing else (it does not
//! clear markers; see [`crate::marker`]).
//!
//! A waiter is a request thread blocked in [`Queue::wait`]. While it waits,
//! that wait keeps its own lease alive, but only while the waiter's
//! connection is: each tick asks the caller's `connected` probe (home's
//! request thread peeks its socket). A waiter whose leaf died stops being
//! refreshed, so the next sweep expires it, loudly; one that reaches the head
//! first is withdrawn there instead of being granted a lease nobody would
//! heartbeat (v0.1 review, F8).
//!
//! Every `sync`, dry run or real, takes an entry; a real one holds it from
//! its plan to its last unit's journal line.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// What home answers for a lease id it does not hold: expired, released, or
/// from before a restart. A leaf matches on it to tell "lost" from "unreachable".
pub const NOT_QUEUED: &str = "is not in the queue";

/// Why a waiter whose connection closed was withdrawn instead of granted.
pub const WAITER_GONE: &str = "the waiter disconnected before reaching the head";

/// How often a waiter re-checks the queue and refreshes its lease.
const WAIT_TICK: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// A dry run (`sync --dry-run`).
    DryRun,
    /// A real `sync`: it writes.
    Sync,
    /// An agent on home holding paths (v0.4).
    Hold,
    /// `replicant rollback` on home, writing a unit back.
    Rollback,
}

#[derive(Debug, Clone)]
struct Entry {
    id: u64,
    host: String,
    kind: EntryKind,
    paths: Vec<String>,
    enqueued_at: u64,
    started_at: Option<u64>,
    /// The lease length it was granted. Heartbeats refresh by this, never
    /// by the fleet's current policy (review P5).
    lease: Duration,
    expires: Instant,
}

/// One entry as `status` shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueView {
    pub id: u64,
    pub host: String,
    pub kind: EntryKind,
    pub paths: Vec<String>,
    pub enqueued_at: u64,
    /// When it reached the head, if it has.
    pub started_at: Option<u64>,
}

/// An entry that stopped heartbeating and was removed.
pub type Expired = QueueView;

#[derive(Debug, Default)]
struct Inner {
    next_id: u64,
    entries: VecDeque<Entry>,
}

#[derive(Debug, Default)]
pub struct Queue {
    inner: Mutex<Inner>,
    cv: Condvar,
}

impl Entry {
    fn view(&self) -> QueueView {
        QueueView {
            id: self.id,
            host: self.host.clone(),
            kind: self.kind.clone(),
            paths: self.paths.clone(),
            enqueued_at: self.enqueued_at,
            started_at: self.started_at,
        }
    }
}

impl Queue {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Enqueue and wait to reach the head; returns the lease id. For a
    /// caller that cannot tell whether it is still connected (tests, holds).
    pub fn acquire(
        &self,
        host: &str,
        kind: EntryKind,
        paths: Vec<String>,
        lease: Duration,
        no_wait: bool,
        max_wait: Duration,
    ) -> Result<u64> {
        let id = self.enqueue(host, kind, paths, lease, no_wait)?;
        self.wait(id, lease, max_wait, &|| true)?;
        Ok(id)
    }

    /// Add an entry at the back; returns its id. `no_wait` fails instead when
    /// anything is ahead.
    pub fn enqueue(
        &self,
        host: &str,
        kind: EntryKind,
        paths: Vec<String>,
        lease: Duration,
        no_wait: bool,
    ) -> Result<u64> {
        let mut q = self.lock();
        if no_wait && let Some(head) = q.entries.front() {
            return Err(Error::Refused(format!(
                "queue busy: {}",
                describe(&head.view())
            )));
        }
        q.next_id += 1;
        let id = q.next_id;
        q.entries.push_back(Entry {
            id,
            host: host.into(),
            kind,
            paths,
            enqueued_at: crate::now_secs(),
            started_at: None,
            lease,
            expires: Instant::now() + lease,
        });
        Ok(id)
    }

    /// Block until entry `id` reaches the head, then start its lease.
    ///
    /// `max_wait` bounds the wait; on timeout the entry is withdrawn.
    /// `connected` is asked once per tick, under the queue lock, so it must be
    /// cheap: the lease is refreshed only while it says yes, and an entry that
    /// reaches the head while it says no is withdrawn ([`WAITER_GONE`]).
    pub fn wait(
        &self,
        id: u64,
        lease: Duration,
        max_wait: Duration,
        connected: &dyn Fn() -> bool,
    ) -> Result<()> {
        let mut q = self.lock();
        let deadline = Instant::now() + max_wait;
        loop {
            let Some(pos) = q.entries.iter().position(|e| e.id == id) else {
                return Err(Error::Refused(
                    "this queue entry was removed while waiting".into(),
                ));
            };
            let alive = connected();
            if pos == 0 && !alive {
                q.entries.remove(0);
                self.cv.notify_all();
                return Err(Error::Refused(WAITER_GONE.into()));
            }
            if pos == 0 {
                let e = &mut q.entries[0];
                e.started_at = Some(crate::now_secs());
                e.expires = Instant::now() + lease;
                return Ok(());
            }
            if Instant::now() >= deadline {
                let ahead = describe(&q.entries[0].view());
                q.entries.remove(pos);
                self.cv.notify_all();
                return Err(Error::Refused(format!(
                    "gave up waiting in the queue: {ahead}"
                )));
            }
            if alive {
                q.entries[pos].expires = Instant::now() + lease;
            }
            q = self
                .cv
                .wait_timeout(q, WAIT_TICK)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Extend a lease by the length it was granted. An unknown id means it
    /// already expired. Reads no policy, so a broken fleet.toml cannot cost
    /// a holder its lease (review P5).
    pub fn heartbeat(&self, id: u64) -> Result<()> {
        let mut q = self.lock();
        match q.entries.iter_mut().find(|e| e.id == id) {
            Some(e) => {
                e.expires = Instant::now() + e.lease;
                Ok(())
            }
            None => Err(Error::Refused(format!(
                "lease {id} {NOT_QUEUED} (expired?)"
            ))),
        }
    }

    /// Finish an entry and let the next one proceed.
    pub fn release(&self, id: u64) -> Result<QueueView> {
        let mut q = self.lock();
        let Some(pos) = q.entries.iter().position(|e| e.id == id) else {
            return Err(Error::Refused(format!(
                "lease {id} {NOT_QUEUED} (expired?)"
            )));
        };
        let e = q.entries.remove(pos).expect("position is in range");
        self.cv.notify_all();
        Ok(e.view())
    }

    /// Remove every entry whose lease has run out as of `now`.
    pub fn expire(&self, now: Instant) -> Vec<Expired> {
        let mut q = self.lock();
        let (dead, live): (Vec<Entry>, Vec<Entry>) =
            q.entries.drain(..).partition(|e| e.expires <= now);
        q.entries = live.into();
        if !dead.is_empty() {
            self.cv.notify_all();
        }
        dead.iter().map(Entry::view).collect()
    }

    pub fn view(&self) -> Vec<QueueView> {
        self.lock().entries.iter().map(Entry::view).collect()
    }
}

/// "twinoaks dry-run work/tundra, started 40s ago".
pub fn describe(v: &QueueView) -> String {
    let kind = match v.kind {
        EntryKind::DryRun => "dry-run",
        EntryKind::Sync => "sync",
        EntryKind::Hold => "hold",
        EntryKind::Rollback => "rollback",
    };
    let age = match v.started_at {
        Some(s) => format!(
            "started {} ago",
            crate::timefmt::ago(crate::now_secs().saturating_sub(s))
        ),
        None => "waiting".into(),
    };
    format!("{} {kind} {}, {age}", v.host, v.paths.join(" "))
}

#[cfg(test)]
#[path = "queue_tests.rs"]
mod tests;
