//! One unit of a sync, in the order PLAN.md's "Interrupted sync" fixes: the
//! target side's marker, the transfer, and only once it finished and was
//! verified the baselines on both sides, the marker cleared, the journal
//! line.
//!
//! The target writes the marker: this leaf for a pull (a removal, a
//! deletion confirmed on home's behalf), home for a push (`BeginPush`).
//! Each side's state in a new baseline is the one it was planned with for
//! the source (a change since is then still a change), and hashed again
//! after the transfer for the target.

use super::{AppliedUnit, Outcome};
use crate::baseline;
use crate::daemon::wire::{HomeReply, HomeRequest};
use crate::marker::{self, Direction, Marker};
use crate::plan::lease::Watch;
use crate::plan::xfer::{Ops, PlannedXfer};
use crate::plan::{Planned, PlannedUnit, diff};
use crate::record::RECORD_VERSION;
use crate::state::UnitState;
use crate::{Error, Result};
use std::time::Instant;

#[path = "apply_step.rs"]
mod plan_step;

#[path = "apply_each.rs"]
mod each;
pub(super) use each::apply_all;
use each::stopped_before;

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
    ops: &Ops<'_>,
    watch: &Watch,
    p: &Planned<'_>,
    u: &PlannedUnit,
    t: &PlannedXfer,
    deleting: bool,
    action: &'static str,
    resumed: bool,
    started: Instant,
) -> Result<Done> {
    let (link, ctx) = (ops.link, ops.ctx);
    let (xfer, seen) = (&t.xfer, &t.target);
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
    // Nothing of a unit is written once the abort is raised, not even its
    // marker; the confirm-delete note above is a round-trip to home.
    if watch.abort().raised() {
        return Ok(Done::Stopped(stopped_before(u, watch)));
    }
    // The marker: before the transfer, on the target.
    let direction = xfer.direction;
    let source_state = match (deleting, direction) {
        (true, _) => UnitState::Empty,
        (false, Direction::Pull) => u.home.clone(),
        (false, Direction::Push) => u.leaf.clone(),
    };
    // What is about to happen must still be what was planned: a dry run now,
    // ahead of every write of this unit, compared to the plan the person
    // confirmed, and every path it lists still what the plan saw there (the
    // list alone reads the same after an edit of a listed path). Either way:
    // a pull must not delete or overwrite leaf work made since (a leaf has no
    // backup), and home backs up what a push replaces from this very list.
    let fresh = ops.dry_run(xfer)?;
    let planned = u.diff.as_ref().map(|d| &d.changes);
    if planned != Some(&diff::summarize(&fresh.itemized).changes)
        || diff::target_moved(seen, &fresh)
    {
        return Ok(Done::Stopped(Outcome::Failed {
            rel: u.rel.clone(),
            why: "it changed since the plan was made; nothing of it was written. Run \
                  `replicant sync` again"
                .into(),
        }));
    }
    // The marker (or home's) is the unit's first write.
    if watch.abort().raised() {
        return Ok(Done::Stopped(stopped_before(u, watch)));
    }
    let mut push = None;
    match direction {
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
            let reply = link.call(HomeRequest::BeginPush {
                lease,
                rel: u.rel.clone(),
                source_state,
                is_file: xfer.is_file,
            })?;
            let HomeReply::Begun { push: id } = reply else {
                return Err(Error::Remote(format!(
                    "unexpected reply to begin-push: {reply:?}"
                )));
            };
            push = Some(id);
        }
    }

    let ran = ops.apply(xfer, &fresh);
    let changes = diff::summarize(&ran.itemized).changes;
    if !ran.ok() {
        // Stopped by whoever raised the abort and said why (the CLI hung
        // up); without a reason, a lost lease.
        let why = match (ran.stopped, watch.abort().why()) {
            (true, Some(w)) => format!("{w}; transfer stopped"),
            (true, None) => "lease lost; transfer stopped".to_string(),
            _ => ran.why(),
        };
        let journaled = link
            .call(HomeRequest::Interrupted {
                rel: u.rel.clone(),
                direction,
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
    // A lease lost while the transfer ran means another entry may have run beside
    // it: what arrived is not known to be whole. The marker stays. The
    // lease only, not the abort: a hangup after exit 0 lost no exclusivity
    // and the transfer finished whole, so the unit finishes (review P2).
    if let Err(e) = watch.check() {
        let why = e.to_string();
        let journaled = link
            .call(HomeRequest::Interrupted {
                rel: u.rel.clone(),
                direction,
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
        false => Some(after(link, ctx, p, u, direction, lease)?),
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
    if direction == Direction::Pull {
        markers.delete(&u.rel)?;
    }
    let mut applied = applied(u, action, resumed, &ran);
    applied.push = push;
    let detail = format!(
        "{action} {} files {} del {} {}",
        applied.files_changed,
        applied.deleted,
        crate::timefmt::elapsed(crate::plan::ms(started)),
        if resumed { "resumed → ok" } else { "ok" },
    );
    expect_done(
        link.call(HomeRequest::Settle {
            lease,
            rel: u.rel.clone(),
            direction: Some(direction),
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
