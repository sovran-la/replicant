//! The dry-run half of a plan: one dry run per unit that would move (the
//! diff of both sides' manifests), and guardrails 1, 3, 4 and 5 against what
//! it reports.

use super::xfer::{Ops, PlannedXfer, Xfer};
use super::{DiffSummary, PlannedUnit, leaf_case_insensitive};
use crate::changes::{ChangeKind, Itemized};
use crate::daemon::wire::HomeSnapshot;
use crate::decide::Decision;
use crate::guard::{self, Refusal};
use crate::marker::{Direction, Marker};
use crate::native::manifest::Manifest;
use crate::native::transfer::Diffed;
use crate::units::{self, Discovered, SideUnit, SubSpec};
use crate::{Result, paths, state};

/// Concurrent dry runs: each walks a unit here and asks home for its side.
const DRY_RUN_WORKERS: usize = 4;

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

/// Whether a path `fresh` would overwrite or delete holds something else than
/// `planned`, the target the plan's own dry run saw. A change list says what
/// happens to a path, not what is there: edited since, `~ path` still reads
/// `~ path`. `fresh`'s list is the plan's by now, and a path it does not list
/// loses nothing.
pub(super) fn target_moved(planned: &Manifest, fresh: &Diffed) -> bool {
    let target = &fresh.target;
    fresh
        .itemized
        .changes
        .iter()
        .any(|c| match (planned.get(&c.path), target.get(&c.path)) {
            (Some(then), Some(now)) => !then.unmoved(now),
            (None, None) => false,
            _ => true,
        })
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
/// transfer each one's dry run used and the target it saw (index-aligned;
/// `None` for the rest): apply runs exactly those, without `--dry-run`, and
/// holds them to that target.
pub(super) fn run(
    ops: &Ops<'_>,
    snap: &HomeSnapshot,
    found: &Discovered,
    union: &[String],
    leaf_units: &[SideUnit],
    planned: &mut [PlannedUnit],
    leaf_markers: &[Marker],
) -> Result<Vec<Option<PlannedXfer>>> {
    let ctx = ops.ctx;
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
    let results = state::parallel_map(
        &jobs,
        DRY_RUN_WORKERS,
        |&i| -> Result<(DiffSummary, Vec<Refusal>, PlannedXfer)> {
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
            let ignored: Vec<String> = l
                .into_iter()
                .chain(h)
                .chain(removed.as_ref())
                .flat_map(|u| u.ignored.iter().cloned())
                .collect();
            let excludes = units::with_ignored(excludes, &ignored);
            let (source, target) = match direction {
                Direction::Pull => (h, l),
                Direction::Push => (l, h),
            };
            let is_file = l.is_some_and(|u| u.is_file)
                || h.is_some_and(|u| u.is_file)
                || (remove && p.leaf_path.is_file());
            let source_absent = remove || source.is_none_or(|u| u.state.is_empty());
            let within = |v: &[String]| -> Vec<String> {
                v.iter()
                    .filter(|r| paths::is_within(r, &p.rel))
                    .cloned()
                    .collect()
            };
            let transfer = Xfer {
                direction,
                rel: p.rel.clone(),
                kind: p.kind,
                leaf_path: p.leaf_path.clone(),
                is_file,
                source_absent,
                excludes,
                union: match p.kind {
                    units::UnitKind::Loose => within(union),
                    units::UnitKind::Repo => vec![],
                },
                worktrees: within(&worktrees),
                ignored,
            };
            let Diffed {
                itemized: it,
                target: seen,
                ..
            } = ops.dry_run(&transfer)?;

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
            // Guardrail 6 (review A3): a resumed pull, removal or confirmed
            // deletion carries a decision made before it was interrupted,
            // which says nothing about what the leaf has written since.
            if direction == Direction::Pull
                && let Some(m) = leaf_markers.iter().find(|m| m.rel == p.rel)
            {
                let late = changed_since(p, transfer.is_file, &it, m.started_at);
                refusals.extend(guard::changed_since_interrupted(&p.rel, m.started_at, late));
            }
            let planned = PlannedXfer {
                xfer: transfer,
                target: seen,
            };
            Ok((summarize(&it), refusals, planned))
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

/// The leaf paths `it` would delete or replace that changed after `since`
/// (whole seconds: anything in that second or later counts). "Changed" is
/// the later of mtime and ctime (review R1): `mv`, `cp -p`, `tar x` and
/// `unzip` carry an old mtime into the unit, but the rename or `utimes`
/// that put it there moves ctime, which no user tool can set back.
/// Directories are left out: their times move whenever an interrupted transfer
/// wrote into them, and their contents are itemized path by path. A repo's
/// `.git` is not: a commit made since moves its refs.
fn changed_since(p: &PlannedUnit, is_file: bool, it: &Itemized, since: u64) -> Vec<String> {
    let since_ns = i128::from(since) * 1_000_000_000;
    it.changes
        .iter()
        .filter(|c| !c.is_dir && matches!(c.kind, ChangeKind::Delete | ChangeKind::Update))
        .filter(|c| {
            let path = match is_file {
                true => p.leaf_path.clone(),
                false => p.leaf_path.join(&c.path),
            };
            std::fs::symlink_metadata(path).is_ok_and(|m| !m.is_dir() && changed_ns(&m) > since_ns)
        })
        .map(|c| c.path.clone())
        .collect()
}

/// The later of a path's mtime and ctime, in nanoseconds since the epoch.
fn changed_ns(m: &std::fs::Metadata) -> i128 {
    use std::os::unix::fs::MetadataExt;
    let ns = |s: i64, n: i64| i128::from(s) * 1_000_000_000 + i128::from(n);
    ns(m.mtime(), m.mtime_nsec()).max(ns(m.ctime(), m.ctime_nsec()))
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

#[cfg(test)]
#[path = "diff_moved_tests.rs"]
mod moved_tests;
