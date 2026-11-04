//! Holding home's queue entry for the length of a plan.
//!
//! A thread heartbeats the lease while the plan runs. A transient failure is
//! retried for as long as the lease could still be alive on home; a definitive
//! "not in the queue" (expired, or home restarted: the queue is in memory)
//! marks the lease **lost**. A plan whose lease was lost fails, because the
//! next entry may already have proceeded alongside it, and any transfer it
//! is running is stopped ([`Watch::abort`]): two transfers must never
//! interleave on home (review F3); home also refuses the next frame of one
//! whose lease is no longer the head of its queue.

use super::HomeLink;
use crate::abort::Abort;
use crate::daemon::queue::NOT_QUEUED;
use crate::daemon::wire::HomeRequest;
use crate::{Error, Result};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

/// Shortest heartbeat interval, so a tiny lease does not spin.
const MIN_EVERY: Duration = Duration::from_millis(200);
/// Shortest wait before retrying a failed heartbeat.
const MIN_RETRY: Duration = Duration::from_millis(50);

/// What the plan can ask about its lease while it runs.
#[derive(Debug, Default)]
pub struct Watch {
    id: u64,
    lost: Mutex<Option<String>>,
    abort: Abort,
}

impl Watch {
    fn lose(&self, why: String) {
        self.lost
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert(why);
        self.abort.raise();
    }

    /// The queue entry: what apply's requests to home name.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Raised when the lease is lost: give it to every transfer the plan runs,
    /// and the running one is stopped.
    pub fn abort(&self) -> &Abort {
        &self.abort
    }

    /// `Err` once the lease is lost; the plan stops at the next check.
    pub fn check(&self) -> Result<()> {
        match &*self.lost.lock().unwrap_or_else(|e| e.into_inner()) {
            None => Ok(()),
            Some(why) => Err(Error::Remote(format!(
                "lease lost: queue entry {} on home ended while this plan was running ({why}); \
                 another entry may have proceeded alongside it, so this plan is void",
                self.id
            ))),
        }
    }
}

fn heartbeat(link: &dyn HomeLink, watch: &Watch, window: Duration, stopped: mpsc::Receiver<()>) {
    let every = (window / 3).max(MIN_EVERY);
    let retry = (every / 4).max(MIN_RETRY);
    // When the last heartbeat that home answered was *sent*: home refreshed
    // the lease no earlier than that.
    let mut alive_since = Instant::now();
    let mut wait = every;
    while let Err(mpsc::RecvTimeoutError::Timeout) = stopped.recv_timeout(wait) {
        let sent = Instant::now();
        match link.call(HomeRequest::Heartbeat { lease: watch.id }) {
            Ok(_) => {
                alive_since = sent;
                wait = every;
            }
            Err(e) if e.to_string().contains(NOT_QUEUED) => {
                watch.lose(e.to_string());
                return;
            }
            Err(e) if alive_since.elapsed() >= window => {
                watch.lose(format!(
                    "no heartbeat reached home for {}s, the whole lease: {e}",
                    window.as_secs()
                ));
                return;
            }
            Err(_) => wait = retry,
        }
    }
}

/// Run `f` while a thread heartbeats `lease` (`(id, secs)`), if there is one.
/// A lost lease fails the result whatever `f` returned. `abort` is the
/// watch's: a lost lease raises it, and so may whoever else holds a clone
/// (the CLI hanging up), which stops the running transfer without voiding the
/// plan.
pub(super) fn with_heartbeat<T>(
    link: &dyn HomeLink,
    lease: Option<(u64, u64)>,
    abort: Abort,
    f: impl FnOnce(&Watch) -> Result<T>,
) -> Result<T> {
    let Some((id, secs)) = lease else {
        return f(&Watch {
            abort,
            ..Watch::default()
        });
    };
    let watch = Watch {
        id,
        lost: Mutex::new(None),
        abort,
    };
    let window = Duration::from_secs(secs.max(1));
    let (stop, stopped) = mpsc::channel::<()>();
    let r = std::thread::scope(|s| {
        s.spawn(|| heartbeat(link, &watch, window, stopped));
        let r = f(&watch);
        drop(stop);
        r
    });
    watch.check()?;
    r
}

/// Give the entry back. A failure is an error: home may still hold the entry
/// (the fleet waits out its lease) or may have lost it (this plan's
/// exclusivity was never guaranteed).
pub(super) fn release(link: &dyn HomeLink, id: u64) -> Result<()> {
    link.call(HomeRequest::Release { lease: id })
        .map(|_| ())
        .map_err(|e| Error::Remote(format!("could not release queue entry {id} on home: {e}")))
}

#[cfg(test)]
#[path = "lease_tests.rs"]
mod tests;
