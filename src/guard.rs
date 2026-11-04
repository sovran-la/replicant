//! The guardrails, as refusals with human-readable reports. PLAN.md,
//! "Guardrails". v0.1 runs 1, 3, 4 and 5; 2 (stale baseline) needs baselines
//! and lands with apply in v0.2. 6 guards a resumed pull (review A3).
//!
//! In a dry run a refusal is reported, not enforced — nothing is applied — but
//! it is computed exactly as an apply would compute it, so `--dry-run` says
//! what the real sync would refuse.

use crate::changes::Itemized;
use crate::marker::Direction;
use crate::paths;
use crate::units::{SideUnit, UnitKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Guard {
    /// 1: the target has uncommitted work the transfer would overwrite.
    DirtyTarget,
    /// 3: the source has a git lock held: a write is in progress.
    SourceMidWrite,
    /// 4: the source has names differing only by case, headed for a
    /// case-insensitive target.
    CaseCollision,
    /// 5: a path outside the host's subscriptions or cap.
    SubscriptionBounds,
    /// 6: a resumed pull would replace or delete leaf paths changed after
    /// the interrupted sync began (review A3). Waived per unit by
    /// `--confirm-delete`.
    ChangedSinceInterrupted,
}

impl Guard {
    pub fn number(self) -> u8 {
        match self {
            Guard::DirtyTarget => 1,
            Guard::SourceMidWrite => 3,
            Guard::CaseCollision => 4,
            Guard::SubscriptionBounds => 5,
            Guard::ChangedSinceInterrupted => 6,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Guard::DirtyTarget => "dirty target",
            Guard::SourceMidWrite => "source mid-write",
            Guard::CaseCollision => "case collision",
            Guard::SubscriptionBounds => "subscription bounds",
            Guard::ChangedSinceInterrupted => "changed here since the interrupted sync",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub guard: Guard,
    pub unit: String,
    /// What exactly would die, or what exactly is held, one item per line.
    pub detail: Vec<String>,
}

impl Refusal {
    pub fn headline(&self) -> String {
        format!(
            "guardrail {} ({}): {}",
            self.guard.number(),
            self.guard.title(),
            self.unit
        )
    }
}

/// Guardrail 1: Uncommitted target work that the transfer would replace or delete.
///
/// `target` is the target side's view of a repo unit; `diff` the dry run in
/// that direction. A dirty path the transfer leaves alone is not at risk: either the
/// source has the same bytes, or the path is excluded.
pub fn dirty_target(target: &SideUnit, diff: &Itemized) -> Option<Refusal> {
    if target.kind != UnitKind::Repo || target.dirty.is_empty() {
        return None;
    }
    let touched: Vec<&str> = diff.touched().collect();
    let dies: Vec<String> = target
        .dirty
        .iter()
        .filter(|d| {
            touched
                .iter()
                .any(|t| paths::is_within(t, d.trim_end_matches('/')))
        })
        .cloned()
        .collect();
    (!dies.is_empty()).then(|| Refusal {
        guard: Guard::DirtyTarget,
        unit: target.rel.clone(),
        detail: dies,
    })
}

/// Guardrail 3: A git lock on the source: someone is writing the repo right now.
pub fn source_mid_write(source: &SideUnit) -> Option<Refusal> {
    (!source.locks.is_empty()).then(|| Refusal {
        guard: Guard::SourceMidWrite,
        unit: source.rel.clone(),
        detail: source.locks.clone(),
    })
}

/// Guardrail 4: Case collisions on home headed for a case-insensitive leaf. Pull only:
/// a case-insensitive leaf cannot hold a collision to push.
pub fn case_collision(
    home: &SideUnit,
    direction: Direction,
    leaf_insensitive: bool,
) -> Option<Refusal> {
    if direction != Direction::Pull || !leaf_insensitive || home.collisions.is_empty() {
        return None;
    }
    Some(Refusal {
        guard: Guard::CaseCollision,
        unit: home.rel.clone(),
        detail: home
            .collisions
            .iter()
            .map(|(a, b)| format!("{a}  ↔  {b}"))
            .collect(),
    })
}

/// Guardrail 5: Whether a unit lies inside the host's subscriptions, outside its
/// carve-outs, and inside its cap.
pub fn bounds(
    rel: &str,
    subs: &[String],
    except: &[String],
    cap: Option<&[String]>,
) -> Option<Refusal> {
    let mut why = Vec::new();
    if !subs.iter().any(|s| paths::is_within(rel, s)) {
        why.push(format!("not inside any subscription ({})", list(subs)));
    }
    if let Some(e) = except.iter().find(|e| paths::is_within(rel, e)) {
        why.push(format!("inside the unsubscribed path {e}"));
    }
    if let Some(cap) = cap.filter(|c| !c.iter().any(|a| paths::is_within(rel, a))) {
        why.push(format!("outside this host's cap ({})", list(cap)));
    }
    (!why.is_empty()).then(|| Refusal {
        guard: Guard::SubscriptionBounds,
        unit: rel.to_string(),
        detail: why,
    })
}

/// Guardrail 5, on what the dry run reports: every path must stay inside the unit.
/// The diff is of the unit's own manifests, so this cannot fire unless
/// something upstream is wrong — which is exactly when it should.
pub fn diff_bounds(unit: &str, diff: &Itemized) -> Option<Refusal> {
    let escapes: Vec<String> = diff
        .changes
        .iter()
        .filter(|c| c.path.starts_with('/') || paths::has_dotdot(&c.path))
        .map(|c| c.path.clone())
        .collect();
    (!escapes.is_empty()).then(|| Refusal {
        guard: Guard::SubscriptionBounds,
        unit: unit.to_string(),
        detail: escapes
            .into_iter()
            .map(|p| format!("the dry run reported a path outside the unit: {p}"))
            .collect(),
    })
}

/// Guardrail 6 (review A3): `late` are the target paths a resume would
/// replace or delete that changed after its marker's `started_at`. The
/// marker records what the source held, not what the target has gained
/// since, so the earlier decision does not cover them.
pub fn changed_since_interrupted(
    unit: &str,
    started_at: u64,
    late: Vec<String>,
) -> Option<Refusal> {
    (!late.is_empty()).then(|| Refusal {
        guard: Guard::ChangedSinceInterrupted,
        unit: unit.to_string(),
        detail: late
            .into_iter()
            .map(|p| {
                format!(
                    "{p}: changed here after the interrupted sync began ({}); resuming would \
                     replace or delete it",
                    crate::timefmt::iso(started_at)
                )
            })
            .collect(),
    })
}

fn list(v: &[String]) -> String {
    if v.is_empty() {
        "none".into()
    } else {
        v.join(", ")
    }
}

#[cfg(test)]
#[path = "guard_tests.rs"]
mod tests;
