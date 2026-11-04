//! Carrying a change list out, in the order the receiver needs: read-only
//! directories it writes into opened, what a dead transfer left, kind changes
//! and directories (on one connection), then the files over every connection,
//! then, only once every file arrived, the deletions and the directories'
//! final modes.
//!
//! Everything but the leaf's own writes goes through [`frame::lists`], so no
//! list is ever one frame too large, however long.

use super::watch::Watch;
use super::{Diffed, Done, Env, Transfer};
use crate::native::conn::Conn;
use crate::native::frame::{self, DATA_VERSION, Frame, Kind};
use crate::native::receive::Receiver;
use crate::native::send::Window;
use crate::native::serve::Replace;
use crate::native::session::{self, Hello};
use crate::native::source::Source;
use crate::native::workers::{self, Shared};
use crate::{Error, Result, paths};
use std::thread::Scope;

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
            watch.closing(c.closer()?);
            if env.abort.raised() {
                return Err(Error::Transfer("stopped".into()));
            }
            session::open(&mut c, &hello)?;
            conns.push(c);
        }
        Ok(conns)
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
        let r = Self::supervised(env, &sh, c.totals(), |watch, s| {
            let mut conns = self.connect(env, n, watch, s)?;
            let mut w = Window::new(&mut conns[0]);
            for f in frame::lists(Kind::Mkdirs, &c.open)? {
                w.call(&f)?;
            }
            for f in frame::lists(Kind::Clean, &d.temps)? {
                w.call(&f)?;
            }
            // What lies under a directory going goes first, as `Deletes`,
            // backed up like any deletion: such a list can be long.
            for (rel, _) in &c.replace {
                for f in frame::lists(Kind::Deletes, &under(rel, &c.delete))? {
                    w.call(&f)?;
                }
            }
            let list: Vec<Replace> = c
                .replace
                .iter()
                .map(|(rel, kind)| Replace {
                    rel: rel.clone(),
                    kind: *kind,
                })
                .collect();
            for f in frame::lists(Kind::Replace, &list)? {
                w.call(&f)?;
            }
            for f in frame::lists(Kind::Mkdirs, &c.mkdir)? {
                w.call(&f)?;
            }
            done.first = true;
            let push = |conn: &mut Conn| workers::push(conn, &src, &sh, env.abort);
            Self::workers(watch, &mut conns, &push)?;
            sh.round(refs);
            Self::workers(watch, &mut conns, &push)?;
            watch.sends_over();
            if env.abort.raised() {
                return Err(Error::Transfer("stopped".into()));
            }
            let mut w = Window::new(&mut conns[0]);
            let mut kept = Vec::new();
            for f in frame::lists(Kind::Deletes, &c.delete)? {
                kept.extend(w.call(&f)?.parse::<Vec<String>>()?);
            }
            done.deleted = true;
            // Deepest first across frames too.
            for f in frame::lists(Kind::DirModes, &c.final_modes(&kept))? {
                w.call(&f)?;
            }
            for conn in &mut conns {
                conn.send(&Frame::new(Kind::Done, vec![]))?;
            }
            // Home's names are durable once each of these comes back: only
            // then may verification, and a baseline, follow.
            for conn in &mut conns {
                conn.recv()?.expect(Kind::Ack)?;
            }
            Ok(())
        });
        done.sent = sent(&sh);
        r
    }

    /// Pull: home sends what the leaf asks for; the leaf writes it.
    pub(super) fn pull(&self, env: &Env<'_>, d: &Diffed, done: &mut Done) -> Result<()> {
        let c = &d.changes;
        let rx = Receiver::open(&self.leaf_path, self.is_file, None, None)?.expecting(&d.target);
        for e in &c.open {
            rx.mkdir(e)?;
        }
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
            let r = Self::supervised(env, &sh, c.totals(), |watch, s| {
                let mut conns = self.connect(env, n, watch, s)?;
                let pull = |conn: &mut Conn| workers::pull(conn, &rx, &sh, env.abort);
                Self::workers(watch, &mut conns, &pull)?;
                sh.round(refs);
                Self::workers(watch, &mut conns, &pull)?;
                for conn in &mut conns {
                    conn.send(&Frame::new(Kind::Done, vec![]))?;
                }
                Ok(())
            });
            done.sent = sent(&sh);
            r?;
        }
        if env.abort.raised() {
            return Err(Error::Transfer("stopped".into()));
        }
        let kept = rx.delete(&c.delete)?;
        done.deleted = true;
        rx.dir_modes(&c.final_modes(&kept))?;
        rx.sync_dirs()
    }
}

fn sent(sh: &Shared) -> std::collections::HashSet<String> {
    let s = sh.sent.lock().unwrap_or_else(|e| e.into_inner());
    s.iter().map(|e| e.rel.clone()).collect()
}
