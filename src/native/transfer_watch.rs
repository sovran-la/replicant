//! What runs beside a transfer's sends: a monitor that every
//! [`PROGRESS_EVERY`] reports the counts and, once the abort is raised or a
//! worker fails, shuts every connection down, so nothing stays blocked on a
//! socket. No failure may leave it running, a panic included: the transfer's
//! scope would wait for it forever, the heartbeat would keep renewing the
//! lease, and home's queue would be held until the daemon was killed. A panic
//! here is an error, and the unit is interrupted like any other.

use super::{Counts, Env, PROGRESS_EVERY, Transfer};
use crate::native::conn::{Conn, Sock};
use crate::native::workers::Shared;
use crate::{Error, Result};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::Scope;
use std::time::{Duration, Instant};

/// How often the monitor looks at the abort.
const POLL: Duration = Duration::from_millis(20);

/// What the monitor watches and stops.
#[derive(Default)]
pub(super) struct Watch {
    closers: Mutex<Vec<Sock>>,
    /// A worker failed: stop the rest.
    halt: AtomicBool,
    /// The sends are over: the monitor reports once more and ends.
    over: AtomicBool,
    first_error: Mutex<Option<Error>>,
}

impl Watch {
    fn fail(&self, e: Error) {
        self.first_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert(e);
        self.halt.store(true, Ordering::SeqCst);
    }

    /// `c` is one more connection to shut when everything stops.
    pub(super) fn closing(&self, c: Sock) {
        self.closers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(c);
    }

    pub(super) fn sends_over(&self) {
        self.over.store(true, Ordering::SeqCst);
    }

    fn shut(&self) {
        let closers = self.closers.lock().unwrap_or_else(|e| e.into_inner());
        closers.iter().for_each(Sock::shutdown);
    }
}

/// Ends the monitor when dropped, however the sends ended.
struct Over<'w>(&'w Watch);

impl Drop for Over<'_> {
    fn drop(&mut self) {
        self.0.sends_over();
    }
}

fn panicked() -> Error {
    Error::Transfer("a transfer thread panicked; the transfer stopped".into())
}

impl Transfer {
    /// Report progress, and shut everything on an abort or a failure, until
    /// the sends are over.
    fn monitor(env: &Env<'_>, watch: &Watch, sh: &Shared, totals: (u64, u64)) {
        let counts = || Counts {
            files_done: sh.files.load(Ordering::Relaxed),
            files_total: totals.0,
            bytes_done: sh.bytes.load(Ordering::Relaxed),
            bytes_total: totals.1,
        };
        let (mut last, mut shown, mut shut) = (Counts::default(), Instant::now(), false);
        while !watch.over.load(Ordering::SeqCst) {
            std::thread::sleep(POLL);
            if !shut && (env.abort.raised() || watch.halt.load(Ordering::SeqCst)) {
                shut = true;
                watch.shut();
            }
            let now = counts();
            if now != last && shown.elapsed() >= PROGRESS_EVERY {
                (env.progress)(now);
                (last, shown) = (now, Instant::now());
            }
        }
        // A failure as the sends ended: what is blocked on a socket still
        // needs waking (home's side of each, in-process).
        if !shut && watch.halt.load(Ordering::SeqCst) {
            watch.shut();
        }
        if counts() != last {
            (env.progress)(counts());
        }
    }

    /// Run `body` with the monitor beside it. Its failure, or its panic, is
    /// this call's error, and the monitor has ended by the time it returns.
    pub(super) fn supervised<'e>(
        env: &'e Env<'e>,
        sh: &Shared,
        totals: (u64, u64),
        body: impl for<'s> FnOnce(&Watch, &'s Scope<'s, 'e>) -> Result<()>,
    ) -> Result<()> {
        let watch = Watch::default();
        // The outer scope is for what `body` starts and outlives the monitor
        // (home's side of each connection, in-process).
        std::thread::scope(|s| {
            std::thread::scope(|inner| {
                let m = inner.spawn(|| Self::monitor(env, &watch, sh, totals));
                let r = {
                    let _over = Over(&watch);
                    let r = catch_unwind(AssertUnwindSafe(|| body(&watch, s)))
                        .unwrap_or_else(|_| Err(panicked()));
                    if r.is_err() {
                        watch.halt.store(true, Ordering::SeqCst);
                    }
                    r
                };
                let _ = m.join();
                // The connections are dropped by now; these copies of their
                // sockets would keep them open, and a shut socket's peer can
                // stay blocked writing to it until its ack timeout.
                watch
                    .closers
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clear();
                r
            })
        })
    }

    /// Run `work` on every connection at once; the first failure stops them
    /// all, and a panic is a failure.
    pub(super) fn workers(
        watch: &Watch,
        conns: &mut [Conn],
        work: &(dyn Fn(&mut Conn) -> Result<()> + Sync),
    ) -> Result<()> {
        std::thread::scope(|s| {
            for c in conns.iter_mut() {
                s.spawn(move || {
                    match catch_unwind(AssertUnwindSafe(|| work(c))) {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => watch.fail(e),
                        Err(_) => watch.fail(panicked()),
                    };
                });
            }
        });
        match watch
            .first_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}
