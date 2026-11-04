//! Home's half of the native data plane: a unit's manifest for the leaf's
//! diff, and the data connections a transfer runs over.
//!
//! A data connection is admitted like a control request (a pinned host's
//! own certificate, never the bootstrap), and only for the host holding
//! the head of the queue under an entry taken for a sync, for a unit home
//! serves it. A push additionally needs its marker on home (`BeginPush`):
//! that names the push whose backup the receiver renames replaced files
//! into. The head of the queue is checked again as the transfer runs, so a
//! lease that expires mid-unit stops home's side too.

use super::Home;
use super::trust::Caller;
use crate::daemon::wire::{HomeReply, HomeRequest};
use crate::exclude::Excludes;
use crate::marker::{self, Direction, Marker};
use crate::native::conn::Conn;
use crate::native::frame::{Frame, Kind};
use crate::native::session::{self, Hello};
use crate::native::{Manifest, serve};
use crate::units::{self, UnitKind};
use crate::{Error, Result, paths};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// Open data connections per (lease, unit).
pub(super) type Slots = Mutex<HashMap<(u64, String), usize>>;

/// The leaf's additions to a unit's excludes: paths inside the unit only,
/// each of which can only leave more out.
struct Extra<'a> {
    pub union: &'a [String],
    pub worktrees: &'a [String],
    pub ignored: &'a [String],
}

impl Home {
    /// [`HomeRequest::Manifest`]'s answer.
    pub(super) fn manifest_request(&self, host: &str, r: HomeRequest) -> Result<HomeReply> {
        let HomeRequest::Manifest {
            lease,
            rel,
            kind,
            is_file,
            union,
            worktrees,
            ignored,
        } = r
        else {
            return Err(Error::Remote(format!("not a manifest request: {r:?}")));
        };
        let extra = Extra {
            union: &union,
            worktrees: &worktrees,
            ignored: &ignored,
        };
        let manifest = self.unit_manifest(host, lease, &rel, kind, is_file, extra)?;
        Ok(HomeReply::Manifest { manifest })
    }

    /// `host`'s unit `rel` as home holds it, for the leaf's diff.
    fn unit_manifest(
        &self,
        host: &str,
        lease: u64,
        rel: &str,
        kind: UnitKind,
        is_file: bool,
        extra: Extra<'_>,
    ) -> Result<Manifest> {
        self.head_of(lease, host, false)?;
        let (spec, _) = self.spec_of(host, rel)?;
        let inside = |v: &[String]| -> Vec<String> {
            v.iter()
                .filter(|r| paths::check_rel(r).is_ok() && paths::is_within(r, rel))
                .cloned()
                .collect()
        };
        let ex = units::excludes(
            &spec,
            rel,
            kind,
            &inside(extra.union),
            &inside(extra.worktrees),
        )?;
        let ignored: Vec<String> = extra
            .ignored
            .iter()
            .filter(|p| paths::check_rel(p.trim_end_matches('/')).is_ok())
            .cloned()
            .collect();
        let ex = units::with_ignored(ex, &ignored);
        let root = self.fleet()?.home_dir.join(rel);
        Manifest::walk(&root, &ex, is_file)
    }

    /// Serve one data connection to its end. Whatever goes wrong is said to
    /// the leaf in a frame, and the connection closes.
    pub fn serve_data(&self, caller: &Caller, mut conn: Conn) {
        let mut j = Vec::new();
        let opened = session::hello(&mut conn).and_then(|h| {
            let s = self.open_session(caller, &h, &mut j)?;
            Ok((h, s))
        });
        let (hello, served) = match opened {
            Ok(o) => o,
            Err(e) => {
                let _ = conn.send(
                    &Frame::json(Kind::Refused, &e.to_string())
                        .unwrap_or_else(|_| Frame::new(Kind::Refused, b"\"refused\"".to_vec())),
                );
                return;
            }
        };
        let key = (hello.lease, hello.rel.clone());
        let _slot = Slot {
            slots: &self.data_slots,
            key,
        };
        let host = served.host.clone();
        let still = || self.at_head(hello.lease, &host);
        let r = conn
            .send(&Frame::new(Kind::Ready, vec![]))
            .and_then(|()| conn.set_timeout(hello.ack_timeout()))
            .and_then(|()| serve::answer(&mut conn, &served, &still));
        if let Err(e) = r {
            let _ = conn.send(
                &Frame::json(Kind::Fail, &e.to_string())
                    .unwrap_or_else(|_| Frame::new(Kind::Fail, b"\"failed\"".to_vec())),
            );
            eprintln!("replicant: data connection for {} {}: {e}", host, hello.rel);
        }
    }

    /// Whether this hello may be served, and what it may touch.
    fn open_session(
        &self,
        caller: &Caller,
        h: &Hello,
        j: &mut Vec<String>,
    ) -> Result<serve::Served> {
        let host = self.admit_data(caller, &h.host, j)?;
        self.at_head(h.lease, &host)?;
        let (spec, _) = self.spec_of(&host, &h.rel)?;
        // What home never sends or writes, whatever the leaf asks: the
        // root's excludes and the host's carve-outs inside the unit.
        let policy: Excludes = units::excludes(&spec, &h.rel, h.kind, &[], &[])?;
        let (root, streams) = {
            let f = self.fleet()?;
            (f.home_dir.join(&h.rel), f.streams)
        };
        let backup = match h.direction {
            Direction::Pull => None,
            Direction::Push => Some(self.push_run_dir(&host, &h.rel)?),
        };
        let mut slots = self.data_slots.lock().unwrap_or_else(|e| e.into_inner());
        let n = slots.entry((h.lease, h.rel.clone())).or_default();
        if *n >= streams as usize {
            return Err(Error::Refused(format!(
                "home accepts {streams} data connections per unit ([daemon] streams)"
            )));
        }
        *n += 1;
        Ok(serve::Served {
            host,
            direction: h.direction,
            root,
            is_file: h.is_file,
            policy,
            backup,
        })
    }

    /// Where a push of `rel` by `host` renames what it replaces: its
    /// current run's backup directory. The push must have begun.
    fn push_run_dir(&self, host: &str, rel: &str) -> Result<PathBuf> {
        let m: Option<Marker> = marker::store(&self.dirs.state()).get(rel)?;
        let id = match m {
            Some(m) if m.host == host && m.direction == Direction::Push => m.push,
            _ => None,
        }
        .ok_or_else(|| {
            Error::Refused(format!(
                "no push of {rel} by {host} has begun on home; nothing may be written there"
            ))
        })?;
        let runs = super::backup::pushes(&self.dirs.state())
            .get::<super::backup::Push>(&id)?
            .map_or(0, |p| p.runs.len());
        Ok(self.push_dir(&id).join(format!("run-{}", runs.max(1))))
    }
}

/// One data connection's place in [`Home::data_slots`], given back when it
/// closes.
struct Slot<'a> {
    slots: &'a Slots,
    key: (u64, String),
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = slots.get_mut(&self.key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                slots.remove(&self.key);
            }
        }
    }
}
