//! The dry-run half of a plan: one rsync `--dry-run` per unit that would move,
//! and guardrails 1, 3, 4 and 5 against what it reports.

use super::{DiffSummary, LeafCtx, PlannedUnit, leaf_case_insensitive};
use crate::daemon::wire::HomeSnapshot;
use crate::decide::Decision;
use crate::guard::{self, Refusal};
use crate::marker::Direction;
use crate::rsync::{Abort, ChangeKind, Client, Itemized, Runner, Transfer};
use crate::units::{self, Discovered, SideUnit, SubSpec};
use crate::{Result, paths, state};

/// Concurrent rsync dry runs. Each is an ssh session to home; a handful is
/// plenty and does not look like an attack to sshd.
const RSYNC_WORKERS: usize = 4;

pub(super) fn spec_for<'s>(specs: &'s [SubSpec], rel: &str) -> Option<&'s SubSpec> {
    specs
        .iter()
        .filter(|s| paths::is_within(rel, &s.rel))
        .max_by_key(|s| s.rel.len())
}

pub(super) fn summarize(it: &Itemized) -> DiffSummary {
    let changes = it
        .changes
        .iter()
        .map(|c| {
            let k = match c.kind {
                ChangeKind::Create => '+',
                ChangeKind::Update => '~',
                ChangeKind::Attr => 'a',
                ChangeKind::Delete => '-',
            };
            format!("{k} {}{}", c.path, if c.is_dir { "/" } else { "" })
        })
        .collect();
    DiffSummary {
        files_changed: it.files_changed(),
        deleted: it.deleted(),
        changes,
        notes: it.notes.clone(),
    }
}

/// Which way a unit's dry run goes, if it gets one. A "deleted on X" unit is
/// diffed the way its deletion would travel, toward the side that still has
/// it, so the human confirming it sees what would go; it still has no
/// [`Decision::direction`], so nothing can carry it out unconfirmed.
pub(super) fn diff_direction(me: &str, d: &Decision) -> Option<Direction> {
    match d {
        Decision::Remove => Some(Direction::Pull),
        Decision::DeletedOn { host } if host == me => Some(Direction::Push),
        Decision::DeletedOn { .. } => Some(Direction::Pull),
        d => d.direction(),
    }
}

/// Fill `diff` and `refusals` on every unit that would move, and return the
/// transfer each one's dry run used (index-aligned; `None` for the rest):
/// apply runs exactly those, without `--dry-run`.
pub(super) fn run(
    ctx: LeafCtx<'_>,
    snap: &HomeSnapshot,
    found: &Discovered,
    union: &[String],
    leaf_units: &[SideUnit],
    planned: &mut [PlannedUnit],
    abort: &Abort,
) -> Result<Vec<Option<Transfer>>> {
    let mut worktrees = found.worktrees();
    worktrees.extend(snap.worktrees.iter().cloned());
    let sub_rels: Vec<String> = snap.specs.iter().map(|s| s.rel.clone()).collect();
    let excepts: Vec<String> = snap.specs.iter().flat_map(|s| s.except.clone()).collect();
    let unserved: Vec<SubSpec> = snap.unserved.iter().map(|u| u.spec.clone()).collect();
    let jobs: Vec<usize> = (0..planned.len())
        .filter(|i| diff_direction(ctx.me, &planned[*i].decision).is_some())
        .collect();

    let mut transfers = vec![None; planned.len()];
    if jobs.is_empty() {
        return Ok(transfers);
    }
    let runner = Runner {
        program: ctx.rsync.into(),
        abort: abort.clone(),
        ..Runner::new(
            ctx.cfg.rsync_timeout(),
            Client::detect()?,
            ctx.scratch_dir.to_path_buf(),
        )
    };
    let results = state::parallel_map(
        &jobs,
        RSYNC_WORKERS,
        |&i| -> Result<(DiffSummary, Vec<Refusal>, Transfer)> {
            let p = &planned[i];
            let remove = p.decision == Decision::Remove;
            let direction = diff_direction(ctx.me, &p.decision).expect("filtered above");
            let l = leaf_units
                .iter()
                .find(|u| u.rel == p.rel && u.kind == p.kind);
            let h = snap
                .units
                .iter()
                .find(|u| u.rel == p.rel && u.kind == p.kind);
            // A unit home no longer serves keeps the excludes home reports
            // for it, so removing a loose unit spares the repos inside it.
            let spec = spec_for(&snap.specs, &p.rel).or_else(|| spec_for(&unserved, &p.rel));
            let excludes = match spec {
                Some(s) => units::excludes(s, &p.rel, p.kind, union, &worktrees)?,
                None => crate::exclude::Excludes::for_root(&[])?,
            };
            // The leaf's copy a removal would delete, looked at before the
            // dry run so its ignored files are spared like any target's.
            let removed = match remove {
                true => remove_target(p)?,
                false => None,
            };
            // Git-ignored paths on either side never move: the source's are
            // not sent, the target's are not deleted for being absent.
            let ignored = l.into_iter().chain(h).chain(removed.as_ref());
            let excludes = units::with_ignored(excludes, ignored.flat_map(|u| &u.ignored));
            let (source, target) = match direction {
                Direction::Pull => (h, l),
                Direction::Push => (l, h),
            };
            let transfer = Transfer {
                direction,
                leaf_path: p.leaf_path.clone(),
                home_path: p.home_path.clone(),
                home_host: ctx.cfg.rsync_host(),
                is_file: l.is_some_and(|u| u.is_file)
                    || h.is_some_and(|u| u.is_file)
                    || (remove && p.leaf_path.is_file()),
                excludes,
                source_absent: remove || source.is_none_or(|u| u.state.is_empty()),
            };
            let it = transfer.dry_run(&runner)?;

            let mut refusals = Vec::new();
            if !remove {
                refusals.extend(guard::bounds(&p.rel, &sub_rels, &excepts, None));
            }
            refusals.extend(guard::diff_bounds(&p.rel, &it));
            refusals.extend(source.and_then(guard::source_mid_write));
            if let Some(h) = h {
                refusals.extend(guard::case_collision(h, direction, leaf_case_insensitive()));
            }
            let dirty_target = match remove {
                true => removed,
                false => target.cloned(),
            };
            refusals.extend(
                dirty_target
                    .as_ref()
                    .and_then(|t| guard::dirty_target(t, &it)),
            );
            Ok((summarize(&it), refusals, transfer))
        },
    );

    for (i, r) in jobs.into_iter().zip(results) {
        let (d, refusals, transfer) = r?;
        planned[i].diff = Some(d);
        planned[i].refusals = refusals;
        transfers[i] = Some(transfer);
    }
    Ok(transfers)
}

/// The leaf's repo facts for a unit being removed, so guardrail 1 can say
/// what uncommitted work the removal would destroy.
fn remove_target(p: &PlannedUnit) -> Result<Option<SideUnit>> {
    if crate::repos::git_marker(&p.leaf_path) != crate::repos::GitMarker::Repo {
        return Ok(None);
    }
    let (f, ignored, _) =
        units::repo_facts(&p.leaf_path, &crate::exclude::Excludes::for_root(&[])?)?;
    Ok(Some(SideUnit {
        rel: p.rel.clone(),
        sub: p.rel.clone(),
        kind: p.kind,
        state: f.state,
        head: Some(f.head),
        dirty: f.dirty,
        locks: f.locks,
        collisions: vec![],
        is_file: false,
        files: f.files,
        ignored,
    }))
}
