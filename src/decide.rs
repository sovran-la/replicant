//! The direction table, per unit. PLAN.md, "One command":
//!
//! ```text
//! no baseline, target empty   → pull (or push, whichever side has content)
//! no baseline, both populated → conflict; `--adopt home|leaf` is the bulk answer
//! home same,    leaf same     → nothing to do
//! home changed, leaf same     → pull
//! home same,    leaf changed  → push
//! both changed                → conflict
//! own marker for exactly this unit → resume that direction (review A1:
//!                                    one above or below it is ignored)
//! ```
//!
//! And, from PLAN rev 5 (review F4):
//!
//! ```text
//! unit vanished on one side since baseline → "deleted on X"; never a plain
//!                                            pull/push, explicit confirmation
//! ```
//!
//! "Vanished" is narrow: Present at baseline, Empty now, and the other side
//! unchanged and still populated. A unit that vanished on one side and changed
//! on the other is still a conflict. Everything becoming Empty is far more
//! often a renamed directory or an unmounted volume than a real delete, and
//! as an ordinary change it would plan a whole-unit delete.
//!
//! And, for a baseline home no longer serves (review F5, [`decide_unserved`]):
//!
//! ```text
//! marker over it            → conflict naming the interrupted sync (blocking)
//! unsubscribed, leaf same    → remove from the leaf
//! unsubscribed, leaf changed → conflict: push first, or unsubscribe --force
//! root missing on home       → home root missing (blocking: fix fleet.toml)
//! cap narrowed, excluded,
//!   or not served            → orphaned: never deleted, never blocking;
//!                              red when the leaf changed since baseline
//! ```
//!
//! Plus three rows the plan implies but does not tabulate:
//!
//! - **both empty, no baseline** → nothing (there is nothing to move);
//! - **no baseline, both populated, identical hashes** → nothing. Calling two
//!   byte-identical trees a conflict would make every path of the Syncthing
//!   cutover a prompt; v0.2 adopts the shared state as the first baseline;
//! - **home marker from another leaf** → quarantined (PLAN.md, "Interrupted
//!   sync": pulls of that path by any other leaf are refused).
//!
//! After the table, a push of this host's that home rolled back holds its
//! unit as a conflict wherever the table would write the leaf, until the
//! host passes `--accept-rollback` (review A2, [`hold_rolled_back`]).
//!
//! Marker rows come first: an interrupted sync of exactly this unit is never
//! a conflict, unless its source vanished since (resuming would then delete
//! the unit, which needs a confirmation the marker does not carry).

use crate::baseline::Baseline;
use crate::marker::{Direction, Marker};
use crate::state::UnitState;
use serde::{Deserialize, Serialize};

mod resolve;
mod rollback;
mod unserved;
pub use resolve::{Resolution, Resolvable, Take, resolvable};
pub use rollback::{Held, RolledBackPush, hold_rolled_back};
pub use unserved::{UnservedInputs, UnservedReason, decide_unserved, files_here};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "decision")]
pub enum Decision {
    Nothing {
        why: String,
    },
    Pull,
    Push,
    Conflict {
        why: String,
    },
    /// A conflict the person resolved by taking `take`'s copy whole: a
    /// push or a pull over what the other side changed ([`resolve`]).
    Resolved {
        take: Take,
    },
    /// Rerun an interrupted transfer in its original direction.
    Resume {
        direction: Direction,
    },
    /// Home holds another leaf's dead push of this path.
    Quarantined {
        by: String,
        since: u64,
    },
    /// Explicitly unsubscribed, and a baseline says this leaf has it,
    /// unchanged: the next sync deletes the leaf's copy (through guardrail 1).
    /// See [`decide_unserved`] for when it is not.
    Remove,
    /// Present at baseline, now empty on `host`, and unchanged on the other
    /// side. Never a plain pull or push: carrying it out removes the whole
    /// unit on the other side, so it needs explicit confirmation, and has no
    /// [`Decision::direction`].
    DeletedOn {
        host: String,
    },
    /// Home no longer serves this path, for a reason that is home's policy
    /// rather than this host's choice (a cap narrowed, an exclude added, a
    /// path dropped from fleet.toml), and nothing is marked over it. Never
    /// moves data and never blocks; `changed` means the leaf holds work
    /// since baseline that home will not accept (rendered red).
    Orphaned {
        reason: UnservedReason,
        #[serde(default)]
        changed: bool,
    },
    /// Home's fleet.toml has no root for this path: almost always a broken
    /// config on home, not a decision. Blocking.
    HomeRootMissing {
        why: String,
    },
}

impl Decision {
    /// The direction a transfer would run, if any.
    pub fn direction(&self) -> Option<Direction> {
        match self {
            Decision::Pull => Some(Direction::Pull),
            Decision::Push => Some(Direction::Push),
            Decision::Resume { direction } => Some(*direction),
            Decision::Resolved { take } => Some(take.direction()),
            _ => None,
        }
    }

    /// Whether this unit needs a human: conflicts, quarantines, and deletions
    /// awaiting confirmation.
    pub fn is_blocking(&self) -> bool {
        matches!(
            self,
            Decision::Conflict { .. }
                | Decision::Quarantined { .. }
                | Decision::DeletedOn { .. }
                | Decision::HomeRootMissing { .. }
        )
    }

    /// Whether applying the plan would do anything for this unit: a
    /// transfer, a deletion, or the bookkeeping `Nothing` rows carry (an
    /// `identical` or `both changed identically` unit records its baseline;
    /// a `both empty` one drops the stale baseline that named it). Only a
    /// unit the baseline already agrees about (`unchanged`), and one home
    /// does not serve, are settled. A plan with no unit to apply is not
    /// worth a question (the "Apply this plan?" over "nothing to do").
    pub fn applies(&self) -> bool {
        match self {
            Decision::Nothing { why } => why != "unchanged",
            Decision::Orphaned { .. } => false,
            _ => true,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Decision::Nothing { why } => format!("nothing ({why})"),
            Decision::Pull => "pull".into(),
            Decision::Push => "push".into(),
            Decision::Conflict { why } => format!("conflict ({why})"),
            Decision::Resolved { take: Take::Leaf } => "push (resolved: this copy)".into(),
            Decision::Resolved { take: Take::Home } => "pull (resolved: home's copy)".into(),
            Decision::Resume { direction } => format!("resume {direction}"),
            Decision::Quarantined { by, .. } => format!("quarantined (dead push by {by})"),
            Decision::Remove => "remove (unsubscribed)".into(),
            Decision::DeletedOn { host } => format!("deleted on {host}"),
            Decision::Orphaned {
                reason,
                changed: false,
            } => format!("orphaned ({reason})"),
            Decision::Orphaned {
                reason,
                changed: true,
            } => format!("orphaned, changed ({reason})"),
            Decision::HomeRootMissing { .. } => "home root missing".into(),
        }
    }
}

/// What one unit's decision depends on.
#[derive(Debug, Clone, Copy)]
pub struct Inputs<'a> {
    /// The unit's path.
    pub rel: &'a str,
    /// This leaf's fleet name.
    pub me: &'a str,
    /// Home's fleet name.
    pub home_name: &'a str,
    pub leaf: &'a UnitState,
    pub home: &'a UnitState,
    pub baseline: Option<&'a Baseline>,
    /// A marker on this leaf (a dead pull). Only one for exactly `rel`
    /// counts.
    pub leaf_marker: Option<&'a Marker>,
    /// A marker on home (a dead push): another host's covering the unit
    /// quarantines it; this host's own counts only for exactly `rel`.
    pub home_marker: Option<&'a Marker>,
}

pub fn decide(i: Inputs<'_>) -> Decision {
    if let Some(m) = i.home_marker.filter(|m| m.host != i.me) {
        return Decision::Quarantined {
            by: m.host.clone(),
            since: m.started_at,
        };
    }
    // This leaf's own marker (a dead pull here, or its dead push on home)
    // resumes exactly its own unit (review A1). One above or below it (a
    // loose unit holding a marked repo, or a repo inside a marked loose unit)
    // says nothing about this unit's direction: resuming it would pull
    // home's copy over work only the leaf has. Such a unit is decided by the
    // table below; the loose transfer already leaves the repos inside it
    // alone, so neither unit's resume needs the other's marker.
    let leaf_own = i.leaf_marker.filter(|m| m.rel == i.rel);
    let home_own = i.home_marker.filter(|m| m.rel == i.rel);
    // A marker is resumed, never read as a conflict, unless the source it
    // was copying from has since vanished: rerunning it now would be a
    // whole-unit delete nobody confirmed (review F4).
    for (m, source) in [(leaf_own, i.home), (home_own, i.leaf)] {
        let Some(m) = m else { continue };
        if source.is_empty() && !m.source_state.is_empty() {
            return Decision::Conflict {
                why: format!(
                    "an interrupted {} by {} is marked here, and its source has vanished \
                     since; restore it, or roll the sync back",
                    m.direction, m.host
                ),
            };
        }
        return Decision::Resume {
            direction: m.direction,
        };
    }

    let Some(b) = i.baseline else {
        return match (i.leaf.is_empty(), i.home.is_empty()) {
            (true, true) => Decision::Nothing {
                why: "both empty".into(),
            },
            (true, false) => Decision::Pull,
            (false, true) => Decision::Push,
            (false, false) if i.leaf == i.home => Decision::Nothing {
                why: "identical".into(),
            },
            (false, false) => Decision::Conflict {
                why: "no baseline, both populated".into(),
            },
        };
    };

    let home_changed = *i.home != b.home;
    let leaf_changed = *i.leaf != b.leaf;
    match (home_changed, leaf_changed) {
        (false, false) => Decision::Nothing {
            why: "unchanged".into(),
        },
        // Changed means it was Present at baseline; the other side still
        // holds it. Carved out of the pull and push rows (review F4).
        (true, false) if i.home.is_empty() && !i.leaf.is_empty() => Decision::DeletedOn {
            host: i.home_name.into(),
        },
        (false, true) if i.leaf.is_empty() && !i.home.is_empty() => {
            Decision::DeletedOn { host: i.me.into() }
        }
        (true, false) => Decision::Pull,
        (false, true) => Decision::Push,
        (true, true) if i.leaf == i.home => Decision::Nothing {
            why: "both changed identically".into(),
        },
        (true, true) => Decision::Conflict {
            why: "both changed since baseline".into(),
        },
    }
}

#[cfg(test)]
#[path = "decide_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "decide_unserved_tests.rs"]
mod unserved_tests;

#[cfg(test)]
#[path = "decide_resume_tests.rs"]
mod resume_tests;
