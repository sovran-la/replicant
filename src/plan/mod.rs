//! The leaf's `status` and `sync --dry-run`: discover, exchange state with
//! home, decide every unit, and (for a dry run) take a queue entry, run the
//! rsync dry runs and the guardrails.
//!
//! Read-only end to end: git runs without optional locks, rsync runs with
//! `--dry-run`, and nothing here writes a baseline, a marker, or a synced path.

mod diff;

use crate::baseline::{self, Baseline};
use crate::config::LeafConfig;
use crate::daemon::queue::QueueView;
use crate::daemon::wire::{
    Enrollment, HomeReply, HomeRequest, HomeSnapshot, ReportedUnit, SubscriptionsView,
};
use crate::decide::{self, Decision};
use crate::guard::Refusal;
use crate::marker::{self, Marker};
use crate::state::UnitState;
use crate::units::{self, Discovered, SideUnit, UnitKind};
use crate::{Error, Result, paths};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// How a leaf reaches home. The daemon's implementation dials over mTLS; tests
/// may call a [`crate::daemon::home::Home`] in-process.
pub trait HomeLink: Sync {
    fn call(&self, request: HomeRequest) -> Result<HomeReply>;
}

/// Who and where this leaf is.
#[derive(Debug, Clone, Copy)]
pub struct LeafCtx<'a> {
    pub me: &'a str,
    pub home_dir: &'a Path,
    pub cfg: &'a LeafConfig,
    pub state_dir: &'a Path,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffSummary {
    pub files_changed: usize,
    pub deleted: usize,
    /// `+ path` created, `~ path` updated, `a path` attributes, `- path` deleted.
    pub changes: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedUnit {
    pub rel: String,
    pub kind: UnitKind,
    pub decision: Decision,
    pub leaf: UnitState,
    pub home: UnitState,
    pub leaf_head: Option<String>,
    pub home_head: Option<String>,
    pub leaf_path: PathBuf,
    pub home_path: PathBuf,
    pub diff: Option<DiffSummary>,
    pub refusals: Vec<Refusal>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timings {
    pub discover_ms: u64,
    pub queue_ms: u64,
    pub home_state_ms: u64,
    pub leaf_state_ms: u64,
    pub rsync_ms: u64,
    pub total_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub me: String,
    pub home: String,
    pub dry_run: bool,
    pub units: Vec<PlannedUnit>,
    /// Subscriptions home will not serve (guardrail 5).
    pub refused: Vec<Refusal>,
    pub timings: Timings,
}

impl Plan {
    /// Whether anything needs a human: a conflict, a quarantine, a refusal.
    pub fn blocked(&self) -> bool {
        !self.refused.is_empty()
            || self
                .units
                .iter()
                .any(|u| u.decision.is_blocking() || !u.refusals.is_empty())
    }

    /// One line per direction, as PLAN.md's `sync` prints it:
    /// `push → thistledown: 3 repos, 41 files changed, 0 deleted`.
    pub fn summary(&self) -> Vec<String> {
        // (units, repos, files changed, deleted) per decision kind.
        let mut by: BTreeMap<&str, (usize, usize, usize, usize)> = BTreeMap::new();
        for u in &self.units {
            let k = key_of(&u.decision);
            if k == "nothing" {
                continue;
            }
            let e = by.entry(k).or_default();
            e.0 += 1;
            e.1 += usize::from(u.kind == UnitKind::Repo);
            if let Some(d) = &u.diff {
                e.2 += d.files_changed;
                e.3 += d.deleted;
            }
        }
        if by.is_empty() {
            return vec![format!("nothing to do with {}", self.home)];
        }
        by.into_iter()
            .map(|(k, (n, repos, files, del))| match k {
                "pull ←" | "push →" if self.dry_run => {
                    format!(
                        "{k} {}: {repos} repos, {files} files changed, {del} deleted",
                        self.home
                    )
                }
                _ => format!("{k}: {n} units"),
            })
            .collect()
    }
}

fn key_of(d: &Decision) -> &'static str {
    match d {
        Decision::Pull
        | Decision::Resume {
            direction: crate::marker::Direction::Pull,
        } => "pull ←",
        Decision::Push | Decision::Resume { .. } => "push →",
        Decision::Remove => "remove",
        Decision::Conflict { .. } => "conflict",
        Decision::Quarantined { .. } => "quarantined",
        Decision::Nothing { .. } => "nothing",
    }
}

/// What `replicant status` shows on a leaf.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeafStatus {
    pub me: String,
    pub home: String,
    pub enrollment: Enrollment,
    pub cap: Option<Vec<String>>,
    pub paths: Vec<String>,
    pub except: Vec<String>,
    pub as_local: BTreeMap<String, PathBuf>,
    pub pending_paths: Vec<String>,
    pub plan: Option<Plan>,
    pub queue: Vec<QueueView>,
}

fn ms(since: Instant) -> u64 {
    since.elapsed().as_millis() as u64
}

pub fn subscriptions(link: &dyn HomeLink) -> Result<SubscriptionsView> {
    match link.call(HomeRequest::Subscriptions)? {
        HomeReply::Subscriptions(v) => Ok(v),
        other => Err(Error::Remote(format!(
            "unexpected reply to subscriptions: {other:?}"
        ))),
    }
}

/// Plan every unit. `dry_run` adds the queue entry, rsync diffs and guards.
pub fn plan(link: &dyn HomeLink, ctx: LeafCtx<'_>, dry_run: bool, no_wait: bool) -> Result<Plan> {
    let total = Instant::now();
    let view = subscriptions(link)?;
    if view.enrollment != Enrollment::Enrolled {
        return Err(Error::Refused(format!(
            "{} is not enrolled on {} ({:?}); nothing to plan",
            ctx.me, view.home, view.enrollment
        )));
    }
    let mut t = Timings::default();
    let local = |rel: &str| ctx.cfg.local_path(rel, ctx.home_dir);
    let started = Instant::now();
    let found = Discovered::run(&view.specs, &local)?;
    t.discover_ms = ms(started);

    let lease = if dry_run {
        let started = Instant::now();
        let reply = link.call(HomeRequest::Acquire { no_wait })?;
        t.queue_ms = ms(started);
        match reply {
            HomeReply::Lease { lease, lease_secs } => Some((lease, lease_secs)),
            other => {
                return Err(Error::Remote(format!(
                    "unexpected reply to acquire: {other:?}"
                )));
            }
        }
    } else {
        None
    };

    let result = diff::with_heartbeat(link, lease, || {
        let started = Instant::now();
        let snap = match link.call(HomeRequest::State {
            leaf_repos: found.repos(),
            case_check: leaf_case_insensitive(),
        })? {
            HomeReply::State(s) => s,
            other => {
                return Err(Error::Remote(format!(
                    "unexpected reply to state: {other:?}"
                )));
            }
        };
        t.home_state_ms = ms(started);
        let started = Instant::now();
        let mut union = found.repos();
        union.extend(snap.repos.iter().cloned());
        union.sort();
        union.dedup();
        let leaf_units = units::snapshot(&found, &union, false)?;
        t.leaf_state_ms = ms(started);
        let mut planned = decide_all(ctx, &snap, &leaf_units)?;
        if dry_run {
            let started = Instant::now();
            diff::run(ctx, &snap, &found, &union, &leaf_units, &mut planned)?;
            t.rsync_ms = ms(started);
        }
        Ok((planned, snap))
    });
    if let Some((id, _)) = lease {
        let _ = link.call(HomeRequest::Release { lease: id });
    }
    let (units, _snap) = result?;
    t.total_ms = ms(total);
    let plan = Plan {
        me: ctx.me.into(),
        home: view.home,
        dry_run,
        units,
        refused: view.refused,
        timings: t,
    };
    let reported = plan
        .units
        .iter()
        .map(|u| ReportedUnit {
            rel: u.rel.clone(),
            decision: u.decision.clone(),
        })
        .collect();
    let event = if dry_run { "dry-run" } else { "status" };
    link.call(HomeRequest::Report {
        event: event.into(),
        units: reported,
        summary: plan.summary().join("; "),
    })?;
    Ok(plan)
}

/// Guardrail 4 applies when this leaf's filesystem folds case: macOS.
pub fn leaf_case_insensitive() -> bool {
    cfg!(target_os = "macos")
}

fn decide_all(
    ctx: LeafCtx<'_>,
    snap: &HomeSnapshot,
    leaf_units: &[SideUnit],
) -> Result<Vec<PlannedUnit>> {
    let baselines: Vec<Baseline> = baseline::store(ctx.state_dir).all()?;
    let leaf_markers: Vec<Marker> = marker::store(ctx.state_dir).all()?;
    let mut out = Vec::new();
    for l in leaf_units {
        let h = snap
            .units
            .iter()
            .find(|h| h.rel == l.rel && h.kind == l.kind);
        let empty = UnitState::Empty;
        let home_state = h.map(|h| &h.state).unwrap_or(&empty);
        let b = baselines.iter().find(|b| b.rel == l.rel);
        let decision = decide::decide(decide::Inputs {
            me: ctx.me,
            leaf: &l.state,
            home: home_state,
            baseline: b,
            leaf_marker: marker::covering(&leaf_markers, &l.rel),
            home_marker: marker::covering(&snap.markers, &l.rel),
        });
        out.push(PlannedUnit {
            rel: l.rel.clone(),
            kind: l.kind,
            decision,
            leaf: l.state.clone(),
            home: home_state.clone(),
            leaf_head: l.head.clone(),
            home_head: h.and_then(|h| h.head.clone()),
            leaf_path: ctx.cfg.local_path(&l.rel, ctx.home_dir),
            home_path: snap.home_dir.join(&l.rel),
            diff: None,
            refusals: vec![],
        });
    }
    // A baseline outside every subscription is a path this leaf was synced
    // and has since unsubscribed: the next sync removes the leaf's copy.
    for b in &baselines {
        let subscribed = snap.specs.iter().any(|s| {
            paths::is_within(&b.rel, &s.rel)
                && !s.except.iter().any(|e| paths::is_within(&b.rel, e))
        });
        if !subscribed {
            out.push(PlannedUnit {
                rel: b.rel.clone(),
                kind: b.kind,
                decision: Decision::Remove,
                leaf: b.leaf.clone(),
                home: UnitState::Empty,
                leaf_head: None,
                home_head: None,
                leaf_path: ctx.cfg.local_path(&b.rel, ctx.home_dir),
                home_path: snap.home_dir.join(&b.rel),
                diff: None,
                refusals: vec![],
            });
        }
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel).then(a.kind.cmp(&b.kind)));
    Ok(out)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
