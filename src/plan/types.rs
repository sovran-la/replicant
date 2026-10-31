//! What a plan is: per-unit decisions, diffs and refusals, and the summary
//! line `sync` prints first.

use crate::daemon::queue::QueueView;
use crate::daemon::wire::Enrollment;
use crate::decide::Decision;
use crate::guard::Refusal;
use crate::state::UnitState;
use crate::units::UnitKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffSummary {
    pub files_changed: usize,
    pub deleted: usize,
    /// `+ path` created, `~ path` updated, `a path` attributes, `- path` deleted.
    pub changes: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedUnit {
    pub rel: String,
    pub kind: UnitKind,
    pub decision: Decision,
    pub leaf: UnitState,
    pub home: UnitState,
    pub leaf_head: Option<String>,
    pub home_head: Option<String>,
    pub leaf_path: PathBuf,
    pub home_path: PathBuf,
    pub diff: Option<DiffSummary>,
    pub refusals: Vec<Refusal>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timings {
    pub discover_ms: u64,
    pub queue_ms: u64,
    pub home_state_ms: u64,
    pub leaf_state_ms: u64,
    pub rsync_ms: u64,
    pub total_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub me: String,
    pub home: String,
    pub dry_run: bool,
    pub units: Vec<PlannedUnit>,
    /// Subscriptions home will not serve (guardrail 5).
    pub refused: Vec<Refusal>,
    pub timings: Timings,
}

impl Plan {
    /// Whether anything needs a human: a conflict, a quarantine, a refusal.
    pub fn blocked(&self) -> bool {
        !self.refused.is_empty()
            || self
                .units
                .iter()
                .any(|u| u.decision.is_blocking() || !u.refusals.is_empty())
    }

    /// One line per direction, as PLAN.md's `sync` prints it:
    /// `push → thistledown: 3 repos, 41 files changed, 0 deleted`.
    pub fn summary(&self) -> Vec<String> {
        // (units, repos, files changed, deleted) per decision kind.
        let mut by: BTreeMap<&str, (usize, usize, usize, usize)> = BTreeMap::new();
        for u in &self.units {
            let k = key_of(&u.decision);
            if k == "nothing" {
                continue;
            }
            let e = by.entry(k).or_default();
            e.0 += 1;
            e.1 += usize::from(u.kind == UnitKind::Repo);
            if let Some(d) = &u.diff {
                e.2 += d.files_changed;
                e.3 += d.deleted;
            }
        }
        if by.is_empty() {
            return vec![format!("nothing to do with {}", self.home)];
        }
        by.into_iter()
            .map(|(k, (n, repos, files, del))| match k {
                "pull ←" | "push →" if self.dry_run => {
                    format!(
                        "{k} {}: {repos} repos, {files} files changed, {del} deleted",
                        self.home
                    )
                }
                _ => format!("{k}: {n} units"),
            })
            .collect()
    }
}

fn key_of(d: &Decision) -> &'static str {
    match d {
        Decision::Pull
        | Decision::Resume {
            direction: crate::marker::Direction::Pull,
        } => "pull ←",
        Decision::Push | Decision::Resume { .. } => "push →",
        Decision::Remove => "remove",
        Decision::Conflict { .. } => "conflict",
        Decision::Quarantined { .. } => "quarantined",
        Decision::Nothing { .. } => "nothing",
    }
}

/// What `replicant status` shows on a leaf.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeafStatus {
    pub me: String,
    pub home: String,
    pub enrollment: Enrollment,
    pub cap: Option<Vec<String>>,
    pub paths: Vec<String>,
    pub except: Vec<String>,
    pub as_local: BTreeMap<String, PathBuf>,
    pub pending_paths: Vec<String>,
    pub plan: Option<Plan>,
    pub queue: Vec<QueueView>,
}
