//! `sync` without `--dry-run`: plan exactly as the dry run does, and then,
//! under the same queue entry, carry the plan out unit by unit. PLAN.md,
//! "Collect, then apply" and "Interrupted sync".
//!
//! Nothing is written unless the whole plan may go: a blocking unit (a
//! conflict, a quarantine, a guardrail refusal, a deletion nobody
//! confirmed, a broken home root), a refused subscription, or a plan that
//! differs from the one the person confirmed stops the sync before the
//! first marker. Then, per unit in plan order (`apply_unit.rs`): the target
//! side's marker, the real rsync, and only after exit 0 the baselines on both
//! sides, the marker cleared, the journal line. The first unit that does not
//! finish stops the sync; its marker stays, so the next `sync` resumes it,
//! and the units after it are untouched.

use super::{HomeLink, LeafCtx, Mode, Plan, PlannedUnit, run};
use crate::Result;
use crate::decide::Decision;
use crate::units::UnitKind;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

#[path = "apply_unit.rs"]
mod unit;

/// What `sync` was told.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncOpts {
    /// Fail instead of waiting behind another queue entry.
    pub no_wait: bool,
    /// Units deleted on one side that may be carried out: each a unit path
    /// (`work/r`) or this leaf's path for it (`/Users/x/work/r`, `~/work/r`).
    pub confirm_delete: Vec<String>,
    /// [`Plan::digest`] of the plan the person confirmed. `None` (`--yes`):
    /// whatever this sync plans is what it applies.
    pub expect: Option<String>,
}

/// How a sync ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// Every unit that moves was applied (possibly none).
    Done,
    /// Something needs a human. Nothing was written.
    Blocked { why: Vec<String> },
    /// The plan is not the one confirmed. Nothing was written.
    Changed,
    /// `rel`'s rsync did not finish (or its lease was lost): the units
    /// before it are done, its marker stays, the rest are untouched.
    Interrupted {
        rel: String,
        why: String,
        /// Whether home journaled it; if not, the leaf does.
        journaled: bool,
    },
    /// A step around `rel`'s rsync failed (home unreachable, a record that
    /// could not be written). Whatever marker it had stays.
    Failed { rel: String, why: String },
}

impl Outcome {
    /// The process exit code: 0 done, 2 blocked (a human is needed, nothing
    /// written), 1 anything else.
    pub fn exit_code(&self) -> i32 {
        match self {
            Outcome::Done => 0,
            Outcome::Blocked { .. } => 2,
            _ => 1,
        }
    }
}

/// One unit `sync` carried out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedUnit {
    pub rel: String,
    pub kind: UnitKind,
    /// `pull`, `push`, `remove`, `delete on home`, `delete here`, `adopt`.
    pub action: String,
    pub resumed: bool,
    /// What rsync reported doing, in the dry run's notation.
    pub changes: Vec<String>,
    pub files_changed: usize,
    pub deleted: usize,
    /// A push's id on home: its backup, for `replicant rollback`.
    #[serde(default)]
    pub push: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReport {
    pub plan: Plan,
    pub outcome: Outcome,
    pub applied: Vec<AppliedUnit>,
}

/// What the apply closure leaves behind, so it survives a lost lease
/// (which fails `run` itself).
#[derive(Default)]
pub(super) struct Stash {
    pub plan: Option<Plan>,
    pub applied: Vec<AppliedUnit>,
    pub outcome: Option<Outcome>,
}

/// Plan, and apply what may be applied. `Err` only when nothing was
/// written (home unreachable, a refused queue entry, a plan that failed);
/// every outcome once apply began is in the report.
pub fn sync(link: &dyn HomeLink, ctx: LeafCtx<'_>, opts: &SyncOpts) -> Result<SyncReport> {
    let stash: Mutex<Stash> = Mutex::default();
    let result = run(link, ctx, Mode::Sync, opts.no_wait, |watch, p| {
        stash.lock().unwrap_or_else(|e| e.into_inner()).plan = Some(p.plan.clone());
        let outcome = match gate(p.plan, opts, ctx.home_dir) {
            Some(o) => o,
            None => unit::apply_all(link, ctx, watch, &p, &stash),
        };
        stash.lock().unwrap_or_else(|e| e.into_inner()).outcome = Some(outcome);
        Ok(())
    });
    let st = std::mem::take(&mut *stash.lock().unwrap_or_else(|e| e.into_inner()));
    let (plan, outcome) = match (result, st.plan, st.outcome) {
        (Ok((plan, ())), _, Some(o)) => (plan, o),
        // Lost after apply began: what was applied stands, and is reported.
        (Err(e), Some(plan), Some(o)) => match o {
            Outcome::Done => (
                plan,
                Outcome::Failed {
                    rel: "-".into(),
                    why: e.to_string(),
                },
            ),
            other => (plan, other),
        },
        (Err(e), _, _) => return Err(e),
        (Ok(_), _, None) => unreachable!("run's closure always records an outcome"),
    };
    let applied = st.applied;
    let summary = summary(&outcome, &applied);
    let ok = outcome == Outcome::Done;
    super::report(link, ctx, "sync", &plan, summary, Some(ok));
    Ok(SyncReport {
        plan,
        outcome,
        applied,
    })
}

/// Why this plan may not be applied as it stands, if it may not.
fn gate(plan: &Plan, opts: &SyncOpts, home_dir: &Path) -> Option<Outcome> {
    let mut why: Vec<String> = plan.refused.iter().map(|r| r.headline()).collect();
    for u in &plan.units {
        why.extend(u.refusals.iter().map(|r| r.headline()));
        let confirmed = opts.confirm_delete.iter().any(|c| confirms(c, u, home_dir));
        match &u.decision {
            Decision::DeletedOn { .. } if confirmed => {}
            Decision::DeletedOn { .. } => why.push(format!(
                "{}: {}; pass --confirm-delete {} or answer y when asked",
                u.rel,
                u.decision.label(),
                u.rel
            )),
            d if d.is_blocking() => why.push(format!("{}: {}", u.rel, d.label())),
            _ => {}
        }
    }
    for c in &opts.confirm_delete {
        let named = plan
            .units
            .iter()
            .any(|u| matches!(u.decision, Decision::DeletedOn { .. }) && confirms(c, u, home_dir));
        if !named {
            why.push(format!(
                "--confirm-delete {c}: no unit in this plan is deleted on one side under that path"
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

/// Whether `entry` (from `--confirm-delete`, or a unit the person said y
/// to) names `u`: its unit path, or this leaf's path for it.
pub fn confirms(entry: &str, u: &PlannedUnit, home_dir: &Path) -> bool {
    let entry = entry.trim_end_matches('/');
    if entry == u.rel {
        return true;
    }
    let local = match entry.strip_prefix("~/") {
        Some(rest) => home_dir.join(rest),
        None => Path::new(entry).to_path_buf(),
    };
    local.is_absolute() && local == u.leaf_path
}

fn summary(outcome: &Outcome, applied: &[AppliedUnit]) -> String {
    let files: usize = applied.iter().map(|a| a.files_changed).sum();
    let del: usize = applied.iter().map(|a| a.deleted).sum();
    let head = format!("{} units, {files} files, {del} del", applied.len());
    match outcome {
        Outcome::Done => format!("{head}; ok"),
        Outcome::Blocked { why } => format!("blocked, nothing written: {}", why.join("; ")),
        Outcome::Changed => "the plan changed since it was confirmed; nothing written".into(),
        Outcome::Interrupted { rel, why, .. } => format!("{head}; interrupted at {rel} ({why})"),
        Outcome::Failed { rel, why } => format!("{head}; failed at {rel}: {why}"),
    }
}

#[cfg(test)]
#[path = "apply_tests.rs"]
pub(crate) mod tests;
