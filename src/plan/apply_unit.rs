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
use crate::marker::{self, Direction, Marker};
use crate::plan::lease::Watch;
use crate::plan::{HomeLink, LeafCtx, Planned, PlannedUnit, diff};
use crate::record::RECORD_VERSION;
use crate::rsync::{ChangeKind, Client, Runner, Transfer};
use crate::state::UnitState;
use crate::{Error, Result};
use std::sync::Mutex;

#[path = "apply_step.rs"]
mod plan_step;
use plan_step::{Step, step};

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
                Ok(Done::Stopped(o)) => return o,
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
    /// The sync stops here, with this outcome.
    Stopped(Outcome),
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
    let mut push = None;
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
                    push: None,
                })?;
            }
        }
        Direction::Push => {
            // Home backs up what this push will replace or delete, so it
            // needs the list as of now, and it must be what was planned.
            let fresh = t.dry_run(runner)?;
            let planned = u.diff.as_ref().map(|d| &d.changes);
            if planned != Some(&diff::summarize(&fresh).changes) {
                return Ok(Done::Stopped(Outcome::Failed {
                    rel: u.rel.clone(),
                    why: "it changed since the plan was made; nothing of it was written. Run \
                          `replicant sync` again"
                        .into(),
                }));
            }
            let backup: Vec<String> = fresh
                .changes
                .iter()
                .filter(|c| !c.is_dir && c.kind != ChangeKind::Create)
                .map(|c| c.path.clone())
                .collect();
            let reply = link.call(HomeRequest::BeginPush {
                lease,
                rel: u.rel.clone(),
                source_state,
                backup,
                is_file: t.is_file,
            })?;
            let HomeReply::Begun { push: id } = reply else {
                return Err(Error::Remote(format!(
                    "unexpected reply to begin-push: {reply:?}"
                )));
            };
            push = Some(id);
        }
    }

    let ran = t.apply(runner)?;
    let changes = diff::summarize(&ran.itemized).changes;
    if !ran.ok() {
        let why = ran.why();
        let journaled = link
            .call(HomeRequest::Interrupted {
                rel: u.rel.clone(),
                direction: t.direction,
                why: why.clone(),
                changes: changes.clone(),
            })
            .is_ok();
        return Ok(Done::Stopped(Outcome::Interrupted {
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
                changes: changes.clone(),
            })
            .is_ok();
        return Ok(Done::Stopped(Outcome::Interrupted {
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
            rollback_accepted: crate::plan::apply::accepts_rollback(ctx, &u.rel, &u.leaf_path),
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
    let mut applied = applied(u, action, resumed, &ran);
    applied.push = push;
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
            changes,
        })?,
        "settle",
    )?;
    Ok(Done::Ran(applied))
}

#[path = "apply_after.rs"]
mod after;
use after::{adopt, after, applied};
