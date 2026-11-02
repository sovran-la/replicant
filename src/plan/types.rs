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
    /// [`crate::units::SideUnit::files`] on each side.
    #[serde(default)]
    pub leaf_files: u64,
    #[serde(default)]
    pub home_files: u64,
}

impl PlannedUnit {
    /// For a "deleted on X" unit, what carrying it out would cost, as
    /// `status` and `sync --dry-run` print it. `me` is the leaf, `home` home.
    pub fn deletion_note(&self, me: &str, home: &str) -> Option<String> {
        let Decision::DeletedOn { host } = &self.decision else {
            return None;
        };
        let (other, n) = if host == me {
            (home, self.home_files)
        } else {
            (me, self.leaf_files)
        };
        let files = if n == 1 { "file" } else { "files" };
        Some(format!(
            "deleted on {host} — would remove {n} {files} on {other}; requires explicit confirmation"
        ))
    }
}

/// How loud a note under a unit is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Needs a human, or warns of work that will not travel: red.
    Alarm,
    /// Information that changes nothing: dim.
    Quiet,
}

impl PlannedUnit {
    /// For a unit home no longer serves, what it means here (review F5).
    pub fn unserved_note(&self) -> Option<(String, Tone)> {
        match &self.decision {
            Decision::Orphaned { reason, changed } => {
                let mut s = format!(
                    "no longer served by home ({reason}); `replicant unsubscribe {}` to remove \
                     here, or re-subscribe",
                    self.leaf_path.display()
                );
                if !changed {
                    return Some((s, Tone::Quiet));
                }
                s.push_str(&format!(
                    "; changed here since baseline ({}) — home will not accept them",
                    crate::decide::files_here(self.leaf_files)
                ));
                Some((s, Tone::Alarm))
            }
            Decision::HomeRootMissing { why } => Some((why.clone(), Tone::Alarm)),
            _ => None,
        }
    }
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
    /// Subscriptions home will not serve because its config is broken (a
    /// root gone): blocking (guardrail 5).
    pub refused: Vec<Refusal>,
    /// Subscriptions home will not serve by its own policy (a cap narrowed,
    /// an exclude added): shown, never blocking (review F5).
    #[serde(default)]
    pub orphaned: Vec<Refusal>,
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
        Decision::DeletedOn { .. } => "deleted, needs confirmation",
        Decision::Orphaned { .. } => "orphaned, not served",
        Decision::HomeRootMissing { .. } => "home root missing",
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
