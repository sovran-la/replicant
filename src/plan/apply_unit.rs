//! One unit of a sync, in the order PLAN.md's "Interrupted sync" fixes: the
//! target side's marker, the real rsync, and only after exit 0 the
//! baselines on both sides, the marker cleared, the journal line.
//!
//! The target writes the marker: this leaf for a pull (a removal, a
//! deletion confirmed on home's behalf), home for a push (`BeginPush`).
//! Each side's state in a new baseline is the one it was planned with for
//! the source (a change since is then still a change), and hashed again
//! after the rsync for the target.

use super::{AppliedUnit, Outcome, Stash};
use crate::baseline::{self, Baseline};
use crate::daemon::wire::{HomeReply, HomeRequest};
use crate::decide::Decision;
use crate::marker::{self, Direction, Marker};
use crate::plan::lease::Watch;
use crate::plan::{HomeLink, LeafCtx, Planned, PlannedUnit};
use crate::record::RECORD_VERSION;
use crate::rsync::{Client, Runner, Transfer};
use crate::state::UnitState;
use crate::{Error, Result};
use std::sync::Mutex;

/// What to do with one planned unit.
enum Step<'p> {
    /// Run its transfer. `deleting`: the source is gone, so afterwards the
    /// baseline is removed rather than written.
    Transfer {
        t: &'p Transfer,
        action: &'static str,
        resumed: bool,
        deleting: bool,
    },
    /// Both sides already agree: record it as the baseline.
    Adopt,
    /// Gone on both sides: the baseline goes too.
    Forget,
    Skip,
}

fn step<'p>(
    ctx: LeafCtx<'_>,
    u: &PlannedUnit,
    t: Option<&'p Transfer>,
    b: Option<&Baseline>,
) -> Step<'p> {
    let Some(t) = t else {
        let agreed = b.is_some_and(|b| b.leaf == u.leaf && b.home == u.home);
        return match &u.decision {
            Decision::Nothing { .. } if !u.leaf.is_empty() && !u.home.is_empty() && !agreed => {
                Step::Adopt
            }
            Decision::Nothing { .. } if u.leaf.is_empty() && u.home.is_empty() && b.is_some() => {
                Step::Forget
            }
            _ => Step::Skip,
        };
    };
    let (action, resumed) = match &u.decision {
        Decision::Pull => ("pull", false),
        Decision::Push => ("push", false),
        Decision::Remove => ("remove", false),
        Decision::DeletedOn { host } if host == ctx.me => ("delete on home", false),
        Decision::DeletedOn { .. } => ("delete here", false),
        Decision::Resume { direction } => (
            match direction {
                Direction::Pull => "pull",
                Direction::Push => "push",
            },
            true,
        ),
        _ => return Step::Skip,
    };
    Step::Transfer {
        t,
        action,
        resumed,
        deleting: t.source_absent,
    }
}

/// Apply every unit of the plan in order, stopping at the first that does
/// not finish. Records each finished unit in `stash` as it goes.
pub(super) fn apply_all(
    link: &dyn HomeLink,
    ctx: LeafCtx<'_>,
    watch: &Watch,
    p: &Planned<'_>,
    stash: &Mutex<Stash>,
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
    let store = baseline::store(ctx.state_dir);
    for (i, u) in p.plan.units.iter().enumerate() {
        let b = match store.get::<Baseline>(&u.rel) {
            Ok(b) => b,
            Err(e) => return failed(u, e),
        };
        let r = match step(ctx, u, p.transfers[i].as_ref(), b.as_ref()) {
            Step::Skip => continue,
            Step::Adopt => adopt(link, ctx, watch, u, false),
            Step::Forget => adopt(link, ctx, watch, u, true),
            Step::Transfer {
                t,
                action,
                resumed,
                deleting,
            } => match transfer(
                link, ctx, watch, p, &runner, u, t, deleting, action, resumed,
            ) {
                Ok(Done::Ran(applied)) => Ok(applied),
                Ok(Done::Interrupted(o)) => return o,
                Err(e) => Err(e),
            },
        };
        match r {
            Ok(a) => stash
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .applied
                .push(a),
            Err(e) => return failed(u, e),
        }
    }
    Outcome::Done
}

fn failed(u: &PlannedUnit, e: Error) -> Outcome {
    Outcome::Failed {
        rel: u.rel.clone(),
        why: e.to_string(),
    }
}

enum Done {
    Ran(AppliedUnit),
    Interrupted(Outcome),
}

fn expect_done(r: HomeReply, what: &str) -> Result<()> {
    match r {
        HomeReply::Done => Ok(()),
        other => Err(Error::Remote(format!(
            "unexpected reply to {what}: {other:?}"
        ))),
    }
}

#[allow(clippy::too_many_arguments)]
fn transfer(
    link: &dyn HomeLink,
    ctx: LeafCtx<'_>,
    watch: &Watch,
    p: &Planned<'_>,
    runner: &Runner,
    u: &PlannedUnit,
    t: &Transfer,
    deleting: bool,
    action: &'static str,
    resumed: bool,
) -> Result<Done> {
    let lease = watch.id();
    let markers = marker::store(ctx.state_dir);
    if let Some(note) = u.deletion_note(ctx.me, &p.plan.home) {
        expect_done(
            link.call(HomeRequest::Note {
                event: "confirm-delete".into(),
                rel: u.rel.clone(),
                detail: format!("{note}: confirmed, carrying it out"),
            })?,
            "note",
        )?;
    }
    // The marker: before the rsync, on the target.
    let source_state = match (deleting, t.direction) {
        (true, _) => UnitState::Empty,
        (false, Direction::Pull) => u.home.clone(),
        (false, Direction::Push) => u.leaf.clone(),
    };
    match t.direction {
        Direction::Pull => {
            // A resumed pull keeps the marker it is resuming.
            if markers.get::<Marker>(&u.rel)?.is_none() {
                markers.put(&Marker {
                    version: RECORD_VERSION,
                    rel: u.rel.clone(),
                    direction: Direction::Pull,
                    host: ctx.me.into(),
                    source_state,
                    started_at: crate::now_secs(),
                })?;
            }
        }
        Direction::Push => expect_done(
            link.call(HomeRequest::BeginPush {
                lease,
                rel: u.rel.clone(),
                source_state,
            })?,
            "begin-push",
        )?,
    }

    let ran = t.apply(runner)?;
    if !ran.ok() {
        let why = ran.why();
        let journaled = link
            .call(HomeRequest::Interrupted {
                rel: u.rel.clone(),
                direction: t.direction,
                why: why.clone(),
            })
            .is_ok();
        return Ok(Done::Interrupted(Outcome::Interrupted {
            rel: u.rel.clone(),
            why,
            journaled,
        }));
    }
    // A lease lost while rsync ran means another entry may have run beside
    // it: what arrived is not known to be whole. The marker stays.
    if let Err(e) = watch.check() {
        let why = e.to_string();
        let journaled = link
            .call(HomeRequest::Interrupted {
                rel: u.rel.clone(),
                direction: t.direction,
                why: why.clone(),
            })
            .is_ok();
        return Ok(Done::Interrupted(Outcome::Interrupted {
            rel: u.rel.clone(),
            why,
            journaled,
        }));
    }

    let agreed = match deleting {
        true => None,
        false => Some(after(link, ctx, p, u, t.direction, lease)?),
    };
    expect_done(
        link.call(HomeRequest::Agree {
            lease,
            rel: u.rel.clone(),
            baseline: agreed.clone(),
        })?,
        "agree",
    )?;
    let store = baseline::store(ctx.state_dir);
    match &agreed {
        Some(b) => store.put(b)?,
        None => store.delete(&u.rel)?,
    }
    if t.direction == Direction::Pull {
        markers.delete(&u.rel)?;
    }
    let applied = applied(u, action, resumed, &ran);
    let detail = format!(
        "{action} {} files {} del {}",
        applied.files_changed,
        applied.deleted,
        if resumed { "resumed → ok" } else { "ok" }
    );
    expect_done(
        link.call(HomeRequest::Settle {
            lease,
            rel: u.rel.clone(),
            direction: Some(t.direction),
            detail,
        })?,
        "settle",
    )?;
    Ok(Done::Ran(applied))
}

#[path = "apply_after.rs"]
mod after;
use after::{adopt, after, applied};
