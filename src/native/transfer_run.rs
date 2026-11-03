//! Carrying a change list out, in the order the receiver needs: what a
//! dead transfer left, kind changes and directories first (on one
//! connection), then the files over every connection, then, only once
//! every file arrived, the deletions and the directories' final modes.
//!
//! A monitor runs beside the workers: every [`PROGRESS_EVERY`] it reports
//! the counts, and once the abort is raised or a worker fails, it shuts
//! every connection down, so nothing stays blocked on a socket.

use super::{Counts, Diffed, Done, Env, PROGRESS_EVERY, Transfer};
use crate::native::conn::{Conn, Sock};
use crate::native::frame::{DATA_VERSION, Frame, Kind};
use crate::native::receive::Receiver;
use crate::native::send::Window;
use crate::native::serve::Replace;
use crate::native::session::{self, Hello};
use crate::native::source::Source;
use crate::native::workers::{self, Shared};
use crate::{Error, Result, paths};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::Scope;
use std::time::{Duration, Instant};

/// How often the monitor looks at the abort.
const POLL: Duration = Duration::from_millis(20);

/// What the monitor watches and stops.
#[derive(Default)]
struct Watch {
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
}

/// The deletions that lie under `rel`, children first.
fn under(rel: &str, delete: &[(String, bool)]) -> Vec<(String, bool)> {
    delete
        .iter()
        .filter(|(p, _)| p != rel && paths::is_within(p, rel))
        .cloned()
        .collect()
}

impl Transfer {
    fn hello(&self, env: &Env<'_>) -> Hello {
        Hello {
            version: DATA_VERSION,
            host: env.me.into(),
            lease: env.lease,
            rel: self.rel.clone(),
            kind: self.kind,
            is_file: self.is_file,
            direction: self.direction,
            ack_timeout_ms: env.ack_timeout.as_millis() as u64,
        }
    }

    /// `n` data connections to home, said hello on; in-process, home's
    /// side of each runs on a thread of `s`.
    fn connect<'s, 'e: 's>(
        &self,
        env: &'e Env<'e>,
        n: usize,
        watch: &Watch,
        s: &'s Scope<'s, 'e>,
    ) -> Result<Vec<Conn>> {
        let hello = self.hello(env);
        let mut conns = Vec::with_capacity(n);
        for _ in 0..n {
            let d = env.link.open_data(&self.rel)?;
            if let Some(serve) = d.serve {
                s.spawn(serve);
            }
            let mut c = d.conn;
            watch
                .closers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(c.closer()?);
            if env.abort.raised() {
                return Err(Error::Transfer("stopped".into()));
            }
            session::open(&mut c, &hello)?;
            conns.push(c);
        }
        Ok(conns)
    }

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
                for c in watch
                    .closers
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                {
                    c.shutdown();
                }
            }
            let now = counts();
            if now != last && shown.elapsed() >= PROGRESS_EVERY {
                (env.progress)(now);
                (last, shown) = (now, Instant::now());
            }
        }
        if counts() != last {
            (env.progress)(counts());
        }
    }

    /// Run `work` on every connection at once; the first failure stops them all.
    fn workers(
        watch: &Watch,
        conns: &mut [Conn],
        work: &(dyn Fn(&mut Conn) -> Result<()> + Sync),
    ) -> Result<()> {
        std::thread::scope(|s| {
            for c in conns.iter_mut() {
                s.spawn(move || {
                    if let Err(e) = work(c) {
                        watch.fail(e);
                    }
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

    /// Push: the leaf sends, home receives (and backs up what it replaces).
    pub(super) fn push(&self, env: &Env<'_>, d: &Diffed, done: &mut Done) -> Result<()> {
        let c = &d.changes;
        if c.is_empty() && d.temps.is_empty() {
            return Ok(());
        }
        let [first, refs] = workers::rounds(&c.send);
        let n = (env.streams as usize).clamp(1, first.len().max(refs.len()).max(1));
        let sh = Shared::new(first);
        let src = Source::open(&self.leaf_path, self.is_file, None)?;
        let watch = Watch::default();
        let r = std::thread::scope(|s| {
            let m = s.spawn(|| Self::monitor(env, &watch, &sh, c.totals()));
            let r = (|| -> Result<()> {
                let mut conns = self.connect(env, n, &watch, s)?;
                let mut w = Window::new(&mut conns[0]);
                if !d.temps.is_empty() {
                    w.call(&Frame::json(Kind::Clean, &d.temps)?)?;
                }
                if !c.replace.is_empty() {
                    let list: Vec<Replace> = c
                        .replace
                        .iter()
                        .map(|(rel, kind)| Replace {
                            rel: rel.clone(),
                            kind: *kind,
                            under: under(rel, &c.delete),
                        })
                        .collect();
                    w.call(&Frame::json(Kind::Replace, &list)?)?;
                }
                if !c.mkdir.is_empty() {
                    w.call(&Frame::json(Kind::Mkdirs, &c.mkdir)?)?;
                }
                done.first = true;
                let push = |conn: &mut Conn| workers::push(conn, &src, &sh, env.abort);
                Self::workers(&watch, &mut conns, &push)?;
                sh.round(refs);
                Self::workers(&watch, &mut conns, &push)?;
                watch.over.store(true, Ordering::SeqCst);
                if env.abort.raised() {
                    return Err(Error::Transfer("stopped".into()));
                }
                let mut w = Window::new(&mut conns[0]);
                if !c.delete.is_empty() {
                    w.call(&Frame::json(Kind::Deletes, &c.delete)?)?;
                }
                done.deleted = true;
                if !c.mkdir.is_empty() {
                    w.call(&Frame::json(Kind::DirModes, &c.mkdir)?)?;
                }
                for conn in &mut conns {
                    conn.send(&Frame::new(Kind::Done, vec![]))?;
                }
                Ok(())
            })();
            watch.over.store(true, Ordering::SeqCst);
            if r.is_err() {
                watch.halt.store(true, Ordering::SeqCst);
            }
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
        });
        done.sent = sent(&sh);
        r
    }

    /// Pull: home sends what the leaf asks for; the leaf writes it.
    pub(super) fn pull(&self, env: &Env<'_>, d: &Diffed, done: &mut Done) -> Result<()> {
        let c = &d.changes;
        let rx = Receiver::open(&self.leaf_path, self.is_file, None, None)?.expecting(&d.target);
        rx.clean(&d.temps)?;
        for (rel, kind) in &c.replace {
            rx.replace(rel, *kind, &under(rel, &c.delete))?;
        }
        for e in &c.mkdir {
            rx.mkdir(e)?;
        }
        done.first = true;
        let [first, refs] = workers::rounds(&c.send);
        let n = (env.streams as usize).clamp(1, first.len().max(refs.len()).max(1));
        let sh = Shared::new(first);
        if !sh.is_empty() || !refs.is_empty() {
            let watch = Watch::default();
            let r = std::thread::scope(|s| {
                let m = s.spawn(|| Self::monitor(env, &watch, &sh, c.totals()));
                let r = (|| -> Result<()> {
                    let mut conns = self.connect(env, n, &watch, s)?;
                    let pull = |conn: &mut Conn| workers::pull(conn, &rx, &sh, env.abort);
                    Self::workers(&watch, &mut conns, &pull)?;
                    sh.round(refs);
                    Self::workers(&watch, &mut conns, &pull)?;
                    for conn in &mut conns {
                        conn.send(&Frame::new(Kind::Done, vec![]))?;
                    }
                    Ok(())
                })();
                watch.over.store(true, Ordering::SeqCst);
                if r.is_err() {
                    watch.halt.store(true, Ordering::SeqCst);
                }
                let _ = m.join();
                watch
                    .closers
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clear();
                r
            });
            done.sent = sent(&sh);
            r?;
        }
        if env.abort.raised() {
            return Err(Error::Transfer("stopped".into()));
        }
        rx.delete(&c.delete)?;
        done.deleted = true;
        rx.dir_modes(&c.mkdir)?;
        rx.sync_dirs()
    }
}

fn sent(sh: &Shared) -> std::collections::HashSet<String> {
    let s = sh.sent.lock().unwrap_or_else(|e| e.into_inner());
    s.iter().map(|e| e.rel.clone()).collect()
}
