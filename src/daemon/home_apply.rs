//! Home's half of `sync`'s apply: the marker on a push (home is its
//! target), home's state of a unit after a push, home's copy of each
//! baseline, and the journal lines. Every write requires the caller to hold
//! the head of the queue, except journaling an interruption, which is
//! evidence and must survive a lost lease.

use super::Home;
use crate::baseline::Baseline;
use crate::daemon::journal::Line;
use crate::daemon::queue::EntryKind;
use crate::daemon::wire::{HomeReply, HomeRequest, NOTE_EVENTS};
use crate::marker::{self, Direction, Marker};
use crate::record::{RECORD_VERSION, Store};
use crate::state::UnitState;
use crate::units::{self, SubSpec};
use crate::{Error, Result, paths};
use std::path::PathBuf;

/// Home's copy of `host`'s baselines: `state/leaf-baselines/<host>/`.
pub fn leaf_baselines(state_dir: &std::path::Path, host: &str) -> Store {
    Store::at(PathBuf::from(state_dir).join("leaf-baselines").join(host))
}

impl Home {
    /// The caller must hold the head of the queue: only then may it write.
    pub(super) fn at_head(&self, lease: u64, host: &str) -> Result<()> {
        self.head_of(lease, host, true)
    }

    /// The caller holds the head of the queue, under an entry taken for a
    /// sync when `writes`, or any entry (a dry run's too) when it only reads.
    pub(super) fn head_of(&self, lease: u64, host: &str, writes: bool) -> Result<()> {
        let q = self.queue.view();
        match q.iter().find(|e| e.id == lease) {
            Some(e)
                if e.host == host
                    && e.started_at.is_some()
                    && (e.kind == EntryKind::Sync || !writes) =>
            {
                Ok(())
            }
            Some(e) if e.host == host && e.started_at.is_some() => Err(Error::Refused(format!(
                "lease {lease} was taken for a {}, which writes nothing",
                crate::daemon::queue::describe(e)
            ))),
            Some(e) if e.host != host => Err(Error::Refused(format!(
                "lease {lease} is {}'s, not {host}'s",
                e.host
            ))),
            _ => Err(Error::Refused(format!(
                "lease {lease} {}: {host} does not hold the head of home's queue, so it may \
                 not write here",
                crate::daemon::queue::NOT_QUEUED
            ))),
        }
    }

    /// The subscription of `host`'s that `rel` is in, as home serves it.
    pub(super) fn spec_of(&self, host: &str, rel: &str) -> Result<(SubSpec, PathBuf)> {
        paths::check_rel(rel).map_err(|why| Error::Path(format!("{rel:?}: {why}")))?;
        let f = self.fleet()?;
        let specs = self.served(&f, host)?.0;
        let spec = specs
            .into_iter()
            .filter(|s| s.covers(rel))
            .max_by_key(|s| s.rel.len())
            .ok_or_else(|| {
                Error::Refused(format!("{rel} is not under anything home serves {host}"))
            })?;
        let abs = f.home_dir.join(&spec.rel);
        Ok((spec, abs))
    }

    pub(super) fn apply_request(
        &self,
        host: &str,
        req: HomeRequest,
        j: &mut Vec<String>,
    ) -> Result<HomeReply> {
        match req {
            HomeRequest::BeginPush {
                lease,
                rel,
                source_state,
                is_file,
            } => {
                self.at_head(lease, host)?;
                let (_, _) = self.spec_of(host, &rel)?;
                let push = self.begin_push(host, &rel, is_file, source_state)?;
                Ok(HomeReply::Begun { push })
            }
            HomeRequest::Rehash {
                lease,
                rel,
                kind,
                union,
            } => {
                self.at_head(lease, host)?;
                let (spec, abs) = self.spec_of(host, &rel)?;
                let union: Vec<String> = union
                    .into_iter()
                    .filter(|r| paths::check_rel(r).is_ok() && paths::is_within(r, &rel))
                    .collect();
                let u = units::rehash(&spec, &abs, &rel, kind, &union)?;
                Ok(HomeReply::Rehashed { state: u.state })
            }
            HomeRequest::Agree {
                lease,
                rel,
                baseline,
                rollback_accepted,
            } => {
                self.at_head(lease, host)?;
                paths::check_rel(&rel).map_err(|why| Error::Path(format!("{rel:?}: {why}")))?;
                let store = leaf_baselines(&self.dirs.state(), host);
                let settles = baseline.is_some() || rollback_accepted;
                match baseline {
                    Some(b) if b.rel != rel => Err(Error::Refused(format!(
                        "a baseline for {} sent as {rel}",
                        b.rel
                    ))),
                    Some(b) => store.put(&Baseline {
                        version: RECORD_VERSION,
                        ..b
                    }),
                    None => store.delete(&rel),
                }?;
                // Agreeing again settles any rollback of this host's push
                // of the unit (review A2). Agreeing that it is gone does
                // not, unless the host accepted the rollback: home's backup
                // may then hold the last copy of what it pushed (R2).
                if settles {
                    self.settle_rollbacks(host, &rel)?;
                }
                Ok(HomeReply::Done)
            }
            HomeRequest::Settle {
                lease,
                rel,
                direction,
                detail,
                changes,
            } => {
                self.at_head(lease, host)?;
                let mut detail = detail;
                if direction == Some(Direction::Push) {
                    if let Some(id) = self.record_run(host, &rel, &changes, true)? {
                        detail.push_str(&format!(" backup:{id}"));
                    }
                    self.clear_push_marker(host, &rel)?;
                }
                self.journal(j, Line::new("sync", host, &rel, detail));
                Ok(HomeReply::Done)
            }
            HomeRequest::Interrupted {
                rel,
                direction,
                why,
                changes,
            } => {
                let mut detail = format!("{direction} interrupted ({why}); marker kept");
                if direction == Direction::Push
                    && let Some(id) = self.record_run(host, &rel, &changes, false)?
                {
                    detail.push_str(&format!(" backup:{id}"));
                }
                self.journal(j, Line::new("sync", host, &rel, detail));
                Ok(HomeReply::Done)
            }
            HomeRequest::Note { event, rel, detail } => {
                if !NOTE_EVENTS.contains(&event.as_str()) {
                    return Err(Error::Refused(format!(
                        "a leaf may not journal {event:?} (only {})",
                        NOTE_EVENTS.join(", ")
                    )));
                }
                self.journal(j, Line::new(&event, host, &rel, detail));
                Ok(HomeReply::Done)
            }
            other => Err(Error::Remote(format!("not an apply request: {other:?}"))),
        }
    }

    /// Mark `rel` as being pushed by `host`, after backing up what the push
    /// will replace or delete. Any marker of someone else's over it, above
    /// or below, is a quarantine. `host`'s own push markers are not: it is
    /// resuming (a loose unit holding its own interrupted repo push resumes
    /// with it), and its marker for exactly `rel` keeps its start and its
    /// push, which gains a run. Returns the push id.
    fn begin_push(
        &self,
        host: &str,
        rel: &str,
        is_file: bool,
        source_state: UnitState,
    ) -> Result<String> {
        let store = marker::store(&self.dirs.state());
        let all: Vec<Marker> = store.all()?;
        let fenced = all
            .iter()
            .find(|m| m.covers(rel) && (m.host != host || m.direction != Direction::Push));
        if let Some(m) = fenced {
            return Err(Error::Refused(format!(
                "{rel} is quarantined on home: an interrupted {} of {} by {} is marked there",
                m.direction, m.rel, m.host
            )));
        }
        let own = all.into_iter().find(|m| m.rel == rel);
        // The run first: if it cannot be opened, nothing is marked.
        let push = self.open_run(host, rel, is_file, own.as_ref())?;
        store.put(&Marker {
            version: RECORD_VERSION,
            rel: rel.into(),
            direction: Direction::Push,
            host: host.into(),
            push: Some(push.clone()),
            backup: None,
            ..own.unwrap_or(Marker {
                version: RECORD_VERSION,
                rel: rel.into(),
                direction: Direction::Push,
                host: host.into(),
                source_state,
                started_at: crate::now_secs(),
                push: None,
                backup: None,
            })
        })?;
        Ok(push)
    }

    /// The push finished: its marker goes. Only `host`'s own.
    fn clear_push_marker(&self, host: &str, rel: &str) -> Result<()> {
        let store = marker::store(&self.dirs.state());
        match store.get::<Marker>(rel)? {
            Some(m) if m.host != host => Err(Error::Refused(format!(
                "the marker over {rel} is {}'s, not {host}'s",
                m.host
            ))),
            _ => store.delete(rel),
        }
    }
}
