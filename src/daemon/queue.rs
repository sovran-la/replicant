//! Home's FIFO queue: syncs and holds, with leases. PLAN.md, "The queue".
//!
//! One invariant: an entry proceeds only when every entry ahead of it is
//! completely done. Nothing interleaves on home.
//!
//! Every entry carries a lease. The holder of the head heartbeats; an entry
//! whose lease runs out is **expired** — removed and reported, so the next
//! entry proceeds. Expiry releases the queue and nothing else (it does not
//! clear markers; see [`crate::marker`]).
//!
//! A waiter is a request thread blocked in [`Queue::acquire`]; while it waits,
//! its own lease is kept alive by that wait, because a connected waiter is by
//! definition not dead.
//!
//! v0.1 takes an entry for every dry run so the mechanism is exercised, and
//! nothing holds one for long.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// How often a waiter re-checks the queue and refreshes its lease.
const WAIT_TICK: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// A sync (in v0.1 always a dry run).
    DryRun,
    /// An agent on home holding paths (v0.4).
    Hold,
}

#[derive(Debug, Clone)]
struct Entry {
    id: u64,
    host: String,
    kind: EntryKind,
    paths: Vec<String>,
    enqueued_at: u64,
    started_at: Option<u64>,
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

    /// Enqueue and wait to reach the head; returns the lease id.
    ///
    /// `no_wait` fails instead of queueing when anything is ahead. `max_wait`
    /// bounds the wait; on timeout the entry is withdrawn.
    pub fn acquire(
        &self,
        host: &str,
        kind: EntryKind,
        paths: Vec<String>,
        lease: Duration,
        no_wait: bool,
        max_wait: Duration,
    ) -> Result<u64> {
        let mut q = self.lock();
        if no_wait {
            if let Some(head) = q.entries.front() {
                return Err(Error::Refused(format!(
                    "queue busy: {}",
                    describe(&head.view())
                )));
            }
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
            expires: Instant::now() + lease,
        });
        let deadline = Instant::now() + max_wait;
        loop {
            let Some(pos) = q.entries.iter().position(|e| e.id == id) else {
                return Err(Error::Refused(
                    "this queue entry was removed while waiting".into(),
                ));
            };
            if pos == 0 {
                let e = &mut q.entries[0];
                e.started_at = Some(crate::now_secs());
                e.expires = Instant::now() + lease;
                return Ok(id);
            }
            if Instant::now() >= deadline {
                let ahead = describe(&q.entries[0].view());
                q.entries.remove(pos);
                self.cv.notify_all();
                return Err(Error::Refused(format!(
                    "gave up waiting in the queue: {ahead}"
                )));
            }
            q.entries[pos].expires = Instant::now() + lease;
            q = self
                .cv
                .wait_timeout(q, WAIT_TICK)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Extend a lease. An unknown id means it already expired.
    pub fn heartbeat(&self, id: u64, lease: Duration) -> Result<()> {
        let mut q = self.lock();
        match q.entries.iter_mut().find(|e| e.id == id) {
            Some(e) => {
                e.expires = Instant::now() + lease;
                Ok(())
            }
            None => Err(Error::Refused(format!(
                "lease {id} is not in the queue (expired?)"
            ))),
        }
    }

    /// Finish an entry and let the next one proceed.
    pub fn release(&self, id: u64) -> Result<QueueView> {
        let mut q = self.lock();
        let Some(pos) = q.entries.iter().position(|e| e.id == id) else {
            return Err(Error::Refused(format!(
                "lease {id} is not in the queue (expired?)"
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
        EntryKind::Hold => "hold",
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
