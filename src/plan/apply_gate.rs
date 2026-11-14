//! Whether a plan may be applied as it stands, and how a command-line
//! entry (`--confirm-delete`, `--accept-rollback`) names a unit.

use super::{Outcome, SyncOpts};
use crate::decide::{Decision, RolledBackPush};
use crate::guard::Guard;
use crate::plan::{LeafCtx, Plan, PlannedUnit};
use std::path::Path;

/// Why this plan may not be applied as it stands, if it may not.
pub(super) fn gate(
    plan: &Plan,
    opts: &SyncOpts,
    home_dir: &Path,
    rolled_back: &[RolledBackPush],
) -> Option<Outcome> {
    let mut why: Vec<String> = plan.refused.iter().map(|r| r.headline()).collect();
    for u in &plan.units {
        let confirmed = opts.confirm_delete.iter().any(|c| confirms(c, u, home_dir));
        why.extend(
            u.refusals
                .iter()
                .filter(|r| !(confirmed && r.guard == Guard::ChangedSinceInterrupted))
                .map(|r| match r.guard {
                    Guard::ChangedSinceInterrupted => format!(
                        "{}; pass --confirm-delete {} to let the resume replace or delete them",
                        r.headline(),
                        u.rel
                    ),
                    _ => r.headline(),
                }),
        );
        match &u.decision {
            Decision::DeletedOn { .. } if confirmed => {}
            Decision::DeletedOn { .. } => why.push(format!(
                "{}: {}; pass --confirm-delete {} or answer y when asked",
                u.rel,
                u.decision.label(),
                u.rel
            )),
            // Skipped by the person: left as it is, the rest goes on.
            Decision::Conflict { .. } if u.resolvable && opts.skip.contains(&u.rel) => {}
            d if d.is_blocking() => why.push(format!("{}: {}", u.rel, d.label())),
            _ => {}
        }
    }
    why.extend(answers(plan, opts));
    for c in &opts.confirm_delete {
        let named = plan
            .units
            .iter()
            .any(|u| confirmable(u) && confirms(c, u, home_dir));
        if !named {
            why.push(format!(
                "--confirm-delete {c}: no unit in this plan is deleted on one side, or would \
                 have a resume replace what changed here since, under that path"
            ));
        }
    }
    for a in &opts.accept_rollback {
        let named = plan.units.iter().any(|u| {
            rolled_back.iter().any(|r| r.unit == u.rel) && names(a, &u.rel, &u.leaf_path, home_dir)
        });
        if !named {
            why.push(format!(
                "--accept-rollback {a}: home rolled back no push of this host's under that path"
            ));
        }
    }
    if !why.is_empty() {
        return Some(Outcome::Blocked { why });
    }
    match &opts.expect {
        Some(d) if *d != plan.digest() => Some(Outcome::Changed),
        _ => None,
    }
}

/// Answers that name no conflict this plan may resolve: a unit that is no
/// longer one (resolved since on the other side, or never), or a path in
/// no plan. A resolution only becomes [`Decision::Resolved`] where it may,
/// so one that did not is the plan having changed under the answer.
fn answers(plan: &Plan, opts: &SyncOpts) -> Vec<String> {
    let unit = |rel: &str| plan.units.iter().find(|u| u.rel == rel);
    let mut why = Vec::new();
    for r in &opts.resolve {
        match unit(&r.rel) {
            Some(u) if matches!(u.decision, Decision::Resolved { take } if take == r.take) => {}
            Some(u) => why.push(format!(
                "{}: answered as a conflict, but it is {} now; run `replicant sync` again",
                r.rel,
                u.decision.label()
            )),
            None => why.push(format!(
                "{}: answered as a conflict, but no unit has that path",
                r.rel
            )),
        }
    }
    for s in &opts.skip {
        match unit(s) {
            Some(u) if u.resolvable && matches!(u.decision, Decision::Conflict { .. }) => {}
            Some(u) => why.push(format!(
                "{s}: skipped as a conflict, but it is {} now; run `replicant sync` again",
                u.decision.label()
            )),
            None => why.push(format!(
                "{s}: skipped as a conflict, but no unit has that path"
            )),
        }
    }
    why
}

/// Whether `--confirm-delete` has anything to confirm on `u`: a deletion
/// on one side, or a resume that would replace or delete what the leaf
/// changed since it was interrupted (guardrail 6, review A3).
pub fn confirmable(u: &PlannedUnit) -> bool {
    matches!(u.decision, Decision::DeletedOn { .. })
        || u.refusals
            .iter()
            .any(|r| r.guard == Guard::ChangedSinceInterrupted)
}

/// Whether `entry` (from `--confirm-delete`, or a unit the person said y
/// to) names `u`: its unit path, or this leaf's path for it.
pub fn confirms(entry: &str, u: &PlannedUnit, home_dir: &Path) -> bool {
    names(entry, &u.rel, &u.leaf_path, home_dir)
}

/// Whether a command-line `entry` names the unit `rel`: its unit path, or
/// its path here (`leaf_path`; absolute or `~/`), component by component.
pub fn names(entry: &str, rel: &str, leaf_path: &Path, home_dir: &Path) -> bool {
    let entry = entry.trim_end_matches('/');
    if entry == rel {
        return true;
    }
    let local = match entry.strip_prefix("~/") {
        Some(rest) => home_dir.join(rest),
        None => Path::new(entry).to_path_buf(),
    };
    local.is_absolute() && local == leaf_path
}

/// Whether this sync's `--accept-rollback` names the unit `rel`.
pub(in crate::plan) fn accepts_rollback(ctx: LeafCtx<'_>, rel: &str, leaf_path: &Path) -> bool {
    ctx.accept_rollback
        .iter()
        .any(|e| names(e, rel, leaf_path, ctx.home_dir))
}
