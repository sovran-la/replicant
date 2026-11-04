//! The leaf's `status`, `sync --dry-run` and `sync`: discover, exchange state
//! with home, decide every unit, and (for a dry run or a sync) take a queue
//! entry, run the dry runs (the diff of both sides' manifests) and the
//! guardrails.
//!
//! Planning is read-only end to end: git runs without optional locks, a dry
//! run only walks and asks home for its manifest, and nothing here writes a
//! baseline, a marker, or a synced path. A real `sync` then applies under the same queue entry
//! ([`apply`]); that is the only code that writes.

pub mod apply;
mod bases;
mod decide_all;
mod diff;
mod lease;
mod open;
pub mod progress;
mod report;
mod types;
mod unserved;
pub mod xfer;

pub use open::{OpenMarker, open_markers};
pub(crate) use report::report;
pub use types::{DiffSummary, LeafStatus, Plan, PlannedUnit, Timings, Tone};
pub use xfer::Plane;

use crate::baseline::{self, Baseline};
use crate::config::LeafConfig;
use crate::daemon::wire::{Enrollment, HomeReply, HomeRequest, HomeSnapshot, SubscriptionsView};
use crate::marker::{self, Marker};
use crate::units::{self, Discovered};
use crate::{Error, Result};
use std::path::Path;
use std::time::Instant;

/// How a leaf reaches home. The daemon's implementation dials over mTLS; tests
/// may call a [`crate::daemon::home::Home`] in-process.
pub trait HomeLink: Sync {
    fn call(&self, request: HomeRequest) -> Result<HomeReply>;

    /// A data connection to home for unit `rel`, not yet said hello on.
    /// In-process links also hand back home's side of it, to be run on a
    /// thread of the caller's while the connection is used.
    fn open_data(&self, rel: &str) -> Result<DataLink<'_>> {
        Err(Error::Remote(format!(
            "this link to home carries no data connections (unit {rel})"
        )))
    }
}

/// [`HomeLink::open_data`]'s connection, and home's side of it when home is
/// in this process.
pub struct DataLink<'a> {
    pub conn: crate::native::conn::Conn,
    pub serve: Option<Box<dyn FnOnce() + Send + 'a>>,
}

/// Who and where this leaf is.
#[derive(Debug, Clone, Copy)]
pub struct LeafCtx<'a> {
    pub me: &'a str,
    pub home_dir: &'a Path,
    pub cfg: &'a LeafConfig,
    pub state_dir: &'a Path,
    /// Which data plane moves the bytes.
    pub plane: Plane,
    /// `--accept-rollback`: units (by unit path or this leaf's path) whose
    /// rolled-back push this host takes home's copy for (review A2).
    pub accept_rollback: &'a [String],
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

/// What a plan is for: `status` (no queue entry, no dry runs), `sync
/// --dry-run`, or a real `sync` (the same as a dry run, and then apply under
/// the same queue entry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Status,
    DryRun,
    Sync,
}

impl Mode {
    /// The journal event home records for it.
    fn event(self) -> &'static str {
        match self {
            Mode::Status => "status",
            Mode::DryRun => "dry-run",
            Mode::Sync => "sync",
        }
    }
}

/// What a plan computed beyond the [`Plan`] itself, for apply: the
/// transfer each moving unit's dry run used and the target it saw
/// (index-aligned with `plan.units`), and the repos of both sides.
pub(crate) struct Planned<'a> {
    pub plan: &'a Plan,
    pub transfers: &'a [Option<xfer::PlannedXfer>],
    pub union: &'a [String],
    pub snap: &'a HomeSnapshot,
    /// Data connections per unit: the lower of both sides' `streams`.
    pub streams: u32,
}

/// Plan every unit. `dry_run` adds the queue entry, the dry runs and guards.
pub fn plan(link: &dyn HomeLink, ctx: LeafCtx<'_>, dry_run: bool, no_wait: bool) -> Result<Plan> {
    let mode = if dry_run { Mode::DryRun } else { Mode::Status };
    let (plan, ()) = run(link, ctx, mode, no_wait, Default::default(), |_, _| Ok(()))?;
    report(
        link,
        ctx,
        mode.event(),
        &plan,
        plan.summary().join("; "),
        None,
    );
    Ok(plan)
}

/// Plan, and then run `then` under the same queue entry, its lease still
/// heartbeated. A lost lease fails the result whatever `then` returned.
/// `abort` stops the plan's dry runs once raised, by a lost lease or
/// by whoever else holds it.
pub(crate) fn run<T>(
    link: &dyn HomeLink,
    ctx: LeafCtx<'_>,
    mode: Mode,
    no_wait: bool,
    abort: crate::abort::Abort,
    then: impl FnOnce(&lease::Watch, Planned<'_>) -> Result<T>,
) -> Result<(Plan, T)> {
    let total = Instant::now();
    let dry_run = mode != Mode::Status;
    let view = subscriptions(link)?;
    if view.enrollment != Enrollment::Enrolled {
        return Err(Error::Refused(format!(
            "{} is not enrolled on {} ({:?}); nothing to plan",
            ctx.me, view.home, view.enrollment
        )));
    }
    let streams = ctx.cfg.streams(view.streams);
    let mut t = Timings::default();
    let local = |rel: &str| ctx.cfg.local_path(rel, ctx.home_dir);
    let started = Instant::now();
    let found = Discovered::run(&view.specs, &local)?;
    t.discover_ms = ms(started);
    let baselines: Vec<Baseline> = baseline::store(ctx.state_dir).all()?;
    bases::check(&found.absent(), &baselines, ctx.me)?;

    let lease = if dry_run {
        let started = Instant::now();
        let reply = link.call(HomeRequest::Acquire {
            no_wait,
            apply: mode == Mode::Sync,
        })?;
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

    // A repo this leaf has a baseline for stays a unit while it is served,
    // even gone from both sides: its baseline must be decided over (and
    // dropped), not left to turn its return into a false conflict.
    let mut leaf_repos = found.repos();
    leaf_repos.extend(
        baselines
            .iter()
            .filter(|b| b.kind == units::UnitKind::Repo)
            .filter(|b| view.specs.iter().any(|s| s.covers(&b.rel)))
            .map(|b| b.rel.clone()),
    );
    leaf_repos.sort();
    leaf_repos.dedup();

    let result = lease::with_heartbeat(link, lease, abort, |watch| {
        let started = Instant::now();
        let snap = match link.call(HomeRequest::State {
            leaf_repos: leaf_repos.clone(),
            case_check: leaf_case_insensitive(),
            baselines: baselines.iter().map(|b| b.rel.clone()).collect(),
        })? {
            HomeReply::State(s) => s,
            other => {
                return Err(Error::Remote(format!(
                    "unexpected reply to state: {other:?}"
                )));
            }
        };
        t.home_state_ms = ms(started);
        let home_absent: Vec<_> = snap
            .absent
            .iter()
            .map(|rel| (rel.clone(), snap.home_dir.join(rel)))
            .collect();
        bases::check(&home_absent, &baselines, &view.home)?;
        let started = Instant::now();
        let mut union = leaf_repos.clone();
        union.extend(snap.repos.iter().cloned());
        union.sort();
        union.dedup();
        let leaf_units = units::snapshot(&found, &union, false)?;
        let leaf_markers: Vec<Marker> = marker::store(ctx.state_dir).all()?;
        let mut planned = decide_all::decide_all(
            ctx,
            &view.home,
            &baselines,
            &snap,
            &leaf_units,
            &leaf_markers,
        );
        let gone = unserved::plan(ctx, &view.home, &snap, &baselines, &leaf_markers)?;
        planned.extend(gone.units);
        planned.sort_by(|a, b| a.rel.cmp(&b.rel).then(a.kind.cmp(&b.kind)));
        t.leaf_state_ms = ms(started);
        let mut transfers = vec![None; planned.len()];
        if dry_run {
            // Do not start a dry run on a lease that is already gone.
            watch.check()?;
            let started = Instant::now();
            union.extend(gone.repos);
            union.sort();
            union.dedup();
            let ops = xfer::Ops::new(link, ctx, watch.id(), watch.abort(), streams);
            transfers = diff::run(
                &ops,
                &snap,
                &found,
                &union,
                &leaf_units,
                &mut planned,
                &leaf_markers,
            )?;
            t.dry_run_ms = ms(started);
        }
        let plan = Plan {
            me: ctx.me.into(),
            home: view.home.clone(),
            dry_run,
            units: planned,
            refused: view.refused.clone(),
            orphaned: view.orphaned.clone(),
            timings: t.clone(),
        };
        let out = then(
            watch,
            Planned {
                plan: &plan,
                transfers: &transfers,
                union: &union,
                snap: &snap,
                streams,
            },
        )?;
        Ok((plan, out))
    });
    if let Some((id, _)) = lease {
        let released = lease::release(link, id);
        match (&result, released) {
            (Ok(_), Err(e)) => return Err(e),
            // The plan's own failure is the one to report.
            (Err(_), Err(e)) => eprintln!("replicant: {e}"),
            (_, Ok(())) => {}
        }
    }
    let (mut plan, out) = result?;
    plan.timings.total_ms = ms(total);
    Ok((plan, out))
}

/// Guardrail 4 applies when this leaf's filesystem folds case: macOS.
pub fn leaf_case_insensitive() -> bool {
    cfg!(target_os = "macos")
}

#[cfg(test)]
#[path = "tests.rs"]
pub(crate) mod tests;
