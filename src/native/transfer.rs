//! One unit's transfer as the leaf runs it: the native data plane behind
//! the same seam rsync's `Transfer` was. A dry run is the diff of the two
//! sides' manifests; applying it carries the change list out over data
//! connections to home and then checks the result: a fresh diff of source
//! against target must leave nothing but directories that hold what the
//! unit does not sync.

use super::manifest::Manifest;
use super::{ChangeList, diff, itemize};
use crate::abort::Abort;
use crate::changes::{ChangeKind, Itemized};
use crate::daemon::wire::{HomeReply, HomeRequest};
use crate::exclude::Excludes;
use crate::marker::Direction;
use crate::plan::HomeLink;
use crate::units::UnitKind;
use crate::{Error, Result};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

#[path = "transfer_run.rs"]
mod run;

/// One unit's transfer.
#[derive(Debug, Clone)]
pub struct Transfer {
    pub direction: Direction,
    pub rel: String,
    pub kind: UnitKind,
    pub leaf_path: PathBuf,
    /// A single-file unit.
    pub is_file: bool,
    /// The source does not exist: a deletion being carried out.
    pub source_absent: bool,
    /// The leaf's walk of its side.
    pub excludes: Excludes,
    /// What home's walk of its side adds to home's own excludes: the
    /// plan's repos and both sides' worktrees inside the unit, both sides'
    /// git-ignored paths.
    pub union: Vec<String>,
    pub worktrees: Vec<String>,
    pub ignored: Vec<String>,
}

/// Live counts of a unit's sends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

/// Where a transfer reaches home, and how it runs.
pub struct Env<'a> {
    pub link: &'a dyn HomeLink,
    pub me: &'a str,
    /// The sync's queue entry.
    pub lease: u64,
    /// Raised: every connection is shut and the unit stops.
    pub abort: &'a Abort,
    /// Data connections per unit.
    pub streams: u32,
    pub ack_timeout: Duration,
    /// Called with the counts at most every [`PROGRESS_EVERY`], and once
    /// more when the sends end.
    pub progress: &'a (dyn Fn(Counts) + Sync),
}

/// How often a unit's progress is reported, at most.
pub const PROGRESS_EVERY: Duration = Duration::from_millis(250);

/// A dry run: what would change, and the list that does it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diffed {
    pub itemized: Itemized,
    pub changes: ChangeList,
    /// The target's temporaries from a dead transfer, to remove first.
    pub temps: Vec<String>,
}

/// What applying a transfer did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    /// What it changed on the target, also when it then failed.
    pub itemized: Itemized,
    /// Why it did not finish; `None`: done and verified.
    pub failure: Option<String>,
    /// It was stopped by its abort.
    pub stopped: bool,
}

impl Ran {
    /// Done and verified: the only outcome after which a baseline may be
    /// written.
    pub fn ok(&self) -> bool {
        self.failure.is_none()
    }

    /// Why it did not succeed, for the journal.
    pub fn why(&self) -> String {
        match (&self.failure, self.stopped) {
            (_, true) => "lease lost; transfer stopped".into(),
            (Some(f), false) => f.clone(),
            (None, false) => "done".into(),
        }
    }
}

impl Transfer {
    fn leaf_manifest(&self) -> Result<Manifest> {
        Manifest::walk(&self.leaf_path, &self.excludes, self.is_file)
    }

    fn home_manifest(&self, env: &Env<'_>) -> Result<Manifest> {
        let reply = env.link.call(HomeRequest::Manifest {
            lease: env.lease,
            rel: self.rel.clone(),
            kind: self.kind,
            is_file: self.is_file,
            union: self.union.clone(),
            worktrees: self.worktrees.clone(),
            ignored: self.ignored.clone(),
        })?;
        match reply {
            HomeReply::Manifest { manifest } => Ok(manifest),
            other => Err(Error::Remote(format!(
                "unexpected reply to manifest: {other:?}"
            ))),
        }
    }

    /// (source, target), as they are now. An absent source is empty and
    /// never asked for.
    pub fn manifests(&self, env: &Env<'_>) -> Result<(Manifest, Manifest)> {
        let (src_home, src_absent) = (self.direction == Direction::Pull, self.source_absent);
        let side = |home: bool| match home {
            true => self.home_manifest(env),
            false => self.leaf_manifest(),
        };
        let source = match src_absent {
            true => Manifest::default(),
            false => side(src_home)?,
        };
        Ok((source, side(!src_home)?))
    }

    /// What the transfer would do now. Writes nothing.
    pub fn dry_run(&self, env: &Env<'_>) -> Result<Diffed> {
        let (source, target) = self.manifests(env)?;
        let changes = diff(&source, &target);
        Ok(Diffed {
            itemized: itemize(&changes, &target),
            changes,
            temps: target.temps,
        })
    }

    /// Carry `d` (this transfer's fresh dry run) out, then verify.
    pub fn apply(&self, env: &Env<'_>, d: &Diffed) -> Ran {
        let mut done = Done::default();
        let r = match self.direction {
            Direction::Push => self.push(env, d, &mut done),
            Direction::Pull => self.pull(env, d, &mut done),
        };
        let r = r.and_then(|()| match env.abort.raised() {
            true => Err(Error::Transfer("stopped".into())),
            false => self.verify(env),
        });
        Ran {
            itemized: done.itemized(d),
            stopped: r.is_err() && env.abort.raised(),
            failure: r.err().map(|e| match env.abort.why() {
                Some(why) if env.abort.raised() => why,
                _ => e.to_string(),
            }),
        }
    }

    /// The target must now be the source, but for directories holding what
    /// the unit does not sync, which no transfer deletes.
    fn verify(&self, env: &Env<'_>) -> Result<()> {
        let (source, target) = self.manifests(env)?;
        let left = diff(&source, &target);
        if left.only_kept_dirs() {
            return Ok(());
        }
        let it = itemize(&left, &target);
        let some: Vec<String> = it.changes.iter().take(5).map(|c| c.path.clone()).collect();
        Err(Error::Transfer(format!(
            "after the transfer {} paths still differ from the source (changed while it was \
             sent?): {}",
            it.changes.len(),
            some.join(", ")
        )))
    }
}

/// Which parts of a transfer were carried out.
#[derive(Debug, Default)]
pub(super) struct Done {
    /// Kind changes and directories.
    pub first: bool,
    /// Files and links renamed into place.
    pub sent: HashSet<String>,
    /// Deletions.
    pub deleted: bool,
}

impl Done {
    /// The dry run's lines for what was done.
    fn itemized(&self, d: &Diffed) -> Itemized {
        let replaced: HashSet<&str> = d.changes.replace.iter().map(|(p, _)| p.as_str()).collect();
        let changes = d
            .itemized
            .changes
            .iter()
            .filter(|c| match c.kind {
                ChangeKind::Delete if replaced.contains(c.path.as_str()) => self.first,
                ChangeKind::Delete => self.deleted,
                _ if c.is_dir => self.first,
                _ => self.sent.contains(&c.path),
            })
            .cloned()
            .collect();
        Itemized {
            changes,
            notes: vec![],
        }
    }
}

#[cfg(test)]
#[path = "transfer_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "transfer_fault_tests.rs"]
mod fault_tests;
