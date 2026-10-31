//! The direction table, per unit. PLAN.md, "One command":
//!
//! ```text
//! no baseline, target empty   → pull (or push, whichever side has content)
//! no baseline, both populated → conflict; `--adopt home|leaf` is the bulk answer
//! home same,    leaf same     → nothing to do
//! home changed, leaf same     → pull
//! home same,    leaf changed  → push
//! both changed                → conflict
//! interrupted marker present  → resume that direction; never a conflict
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
//! Marker rows come first: an interrupted sync is never a conflict.

use crate::baseline::Baseline;
use crate::marker::{Direction, Marker};
use crate::state::UnitState;
use serde::{Deserialize, Serialize};

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
    /// Rerun an interrupted rsync in its original direction.
    Resume {
        direction: Direction,
    },
    /// Home holds another leaf's dead push of this path.
    Quarantined {
        by: String,
        since: u64,
    },
    /// No longer subscribed and a baseline says this leaf has it: the next
    /// sync deletes the leaf's copy (through guardrail 1).
    Remove,
}

impl Decision {
    /// The direction rsync would run, if any.
    pub fn direction(&self) -> Option<Direction> {
        match self {
            Decision::Pull => Some(Direction::Pull),
            Decision::Push => Some(Direction::Push),
            Decision::Resume { direction } => Some(*direction),
            _ => None,
        }
    }

    /// Whether this unit needs a human: conflicts and quarantines.
    pub fn is_blocking(&self) -> bool {
        matches!(
            self,
            Decision::Conflict { .. } | Decision::Quarantined { .. }
        )
    }

    pub fn label(&self) -> String {
        match self {
            Decision::Nothing { why } => format!("nothing ({why})"),
            Decision::Pull => "pull".into(),
            Decision::Push => "push".into(),
            Decision::Conflict { why } => format!("conflict ({why})"),
            Decision::Resume { direction } => format!("resume {direction}"),
            Decision::Quarantined { by, .. } => format!("quarantined (dead push by {by})"),
            Decision::Remove => "remove (unsubscribed)".into(),
        }
    }
}

/// What one unit's decision depends on.
#[derive(Debug, Clone, Copy)]
pub struct Inputs<'a> {
    /// This leaf's fleet name.
    pub me: &'a str,
    pub leaf: &'a UnitState,
    pub home: &'a UnitState,
    pub baseline: Option<&'a Baseline>,
    /// A marker on this leaf covering the unit (a dead pull).
    pub leaf_marker: Option<&'a Marker>,
    /// A marker on home covering the unit (a dead push).
    pub home_marker: Option<&'a Marker>,
}

pub fn decide(i: Inputs<'_>) -> Decision {
    if let Some(m) = i.home_marker.filter(|m| m.host != i.me) {
        return Decision::Quarantined {
            by: m.host.clone(),
            since: m.started_at,
        };
    }
    if let Some(m) = i.leaf_marker {
        return Decision::Resume {
            direction: m.direction,
        };
    }
    if let Some(m) = i.home_marker {
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
