//! Apply's loop: every unit of the plan in order, each announced as it
//! starts and as it ends ([`Progress`]), stopping at the first that does not
//! finish.

use super::super::{AppliedUnit, Outcome, Progress, ProgressEvent, Stash, UnitOutcome};
use super::plan_step::{Step, step};
use super::{Done, adopt, failed, transfer};
use crate::baseline::{self, Baseline};
use crate::marker::Direction;
use crate::plan::lease::Watch;
use crate::plan::{HomeLink, LeafCtx, Planned, PlannedUnit, ms};
use crate::rsync::{Client, Runner};
use std::sync::Mutex;
use std::time::Instant;

/// Apply every unit of the plan in order, stopping at the first that does
/// not finish. Records each finished unit in `stash` as it goes.
pub(in crate::plan) fn apply_all(
    link: &dyn HomeLink,
    ctx: LeafCtx<'_>,
    watch: &Watch,
    p: &Planned<'_>,
    stash: &Mutex<Stash>,
    progress: &Progress<'_>,
) -> Outcome {
    let runner = match Client::detect() {
        Ok(client) => Runner {
            program: ctx.rsync.into(),
            abort: watch.abort().clone(),
            ..Runner::new(
                ctx.cfg.rsync_timeout(),
                client,
                ctx.scratch_dir.to_path_buf(),
            )
        },
        Err(e) => {
            return Outcome::Failed {
                rel: "-".into(),
                why: e.to_string(),
            };
        }
    };
    // What each unit will do, first, so the count is known before the first
    // starts. A baseline that cannot be read fails its unit when it is
    // reached, as before: the units ahead of it still go.
    let store = baseline::store(ctx.state_dir);
    let mut work = Vec::new();
    for (i, u) in p.plan.units.iter().enumerate() {
        match store.get::<Baseline>(&u.rel) {
            Ok(b) => match step(ctx, u, p.transfers[i].as_ref(), b.as_ref()) {
                Step::Skip => {}
                s => work.push((u, Ok(s))),
            },
            Err(e) => work.push((u, Err(e))),
        }
    }
    let total = work.len();
    for (n, (u, s)) in work.into_iter().enumerate() {
        let s = match s {
            Ok(s) => s,
            Err(e) => return failed(u, e),
        };
        let (action, direction) = match &s {
            Step::Transfer { t, action, .. } => (*action, Some(t.direction)),
            Step::Adopt => ("adopt", None),
            Step::Forget => ("forget", None),
            Step::Skip => unreachable!("skipped units are not in the work"),
        };
        progress.emit(ProgressEvent::UnitStarted {
            rel: u.rel.clone(),
            action: action.into(),
            direction,
            index: n + 1,
            total,
        });
        let started = Instant::now();
        let r = match s {
            Step::Adopt => adopt(link, ctx, watch, u, false).map(Done::Ran),
            Step::Forget => adopt(link, ctx, watch, u, true).map(Done::Ran),
            Step::Transfer {
                t,
                action,
                resumed,
                deleting,
            } => transfer(
                link, ctx, watch, p, &runner, u, t, deleting, action, resumed, started,
            ),
            Step::Skip => unreachable!("skipped units are not in the work"),
        };
        let done = UnitEnd {
            u,
            action,
            direction,
            elapsed_ms: ms(started),
        };
        match r {
            Ok(Done::Ran(mut a)) => {
                a.elapsed_ms = done.elapsed_ms;
                progress.emit(done.event(Some(&a), UnitOutcome::Applied));
                stash
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .applied
                    .push(a);
            }
            Ok(Done::Stopped(o)) => {
                progress.emit(done.event(None, unit_outcome(&o)));
                return o;
            }
            Err(e) => {
                let o = failed(u, e);
                progress.emit(done.event(None, unit_outcome(&o)));
                return o;
            }
        }
    }
    Outcome::Done
}

/// What every `UnitDone` of one unit shares.
struct UnitEnd<'u> {
    u: &'u PlannedUnit,
    action: &'static str,
    direction: Option<Direction>,
    elapsed_ms: u64,
}

impl UnitEnd<'_> {
    fn event(&self, a: Option<&AppliedUnit>, outcome: UnitOutcome) -> ProgressEvent {
        ProgressEvent::UnitDone {
            rel: self.u.rel.clone(),
            action: self.action.into(),
            direction: self.direction,
            files_changed: a.map_or(0, |a| a.files_changed),
            deleted: a.map_or(0, |a| a.deleted),
            elapsed_ms: self.elapsed_ms,
            push: a.and_then(|a| a.push.clone()),
            outcome,
        }
    }
}

fn unit_outcome(o: &Outcome) -> UnitOutcome {
    match o {
        Outcome::Interrupted { why, .. } => UnitOutcome::Interrupted { why: why.clone() },
        Outcome::Failed { why, .. } => UnitOutcome::Failed { why: why.clone() },
        other => UnitOutcome::Failed {
            why: format!("{other:?}"),
        },
    }
}
