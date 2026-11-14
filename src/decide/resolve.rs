//! Resolving a conflict by hand (PLAN.md, "Conflicts"; rev 22): the person
//! answering `sync`'s question for one unit takes one side's copy whole.
//!
//! Only the table's own conflicts can be resolved this way: both sides
//! changed since the baseline, or no baseline and both populated, with both
//! sides holding something. The others say what to do instead: an
//! interrupted sync whose source vanished (restore it, or roll it back), a
//! push home rolled back (`--accept-rollback`), work on a path home no
//! longer serves (push first, or `unsubscribe --force`). Taking an empty
//! side would be a whole-unit delete nobody confirmed as one.
//!
//! A resolved unit is a [`super::Decision::Resolved`]: a push (this leaf's
//! copy) or a pull (home's), whose transfer overwrites what the other side
//! changed. That is the point of the answer, so guardrail 1 does not refuse
//! it; what it replaces is kept instead, on home by the push's backup as
//! always, on the leaf by a backup of its own (rev 22).

use crate::marker::{Direction, Marker};
use crate::state::UnitState;
use serde::{Deserialize, Serialize};

/// Whose copy a resolved conflict takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Take {
    /// This leaf's: a push.
    Leaf,
    /// Home's: a pull.
    Home,
}

impl Take {
    pub fn direction(self) -> Direction {
        match self {
            Take::Leaf => Direction::Push,
            Take::Home => Direction::Pull,
        }
    }
}

/// One answer to a conflict: unit `rel` takes `take`'s copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    pub rel: String,
    pub take: Take,
}

/// What [`resolvable`] looks at.
#[derive(Debug, Clone, Copy)]
pub struct Resolvable<'a> {
    pub rel: &'a str,
    /// The direction table's own decision for the unit.
    pub table: &'a super::Decision,
    /// The decision once a rolled-back push held it.
    pub held: &'a super::Decision,
    pub leaf: &'a UnitState,
    pub home: &'a UnitState,
    pub leaf_marker: Option<&'a Marker>,
    pub home_marker: Option<&'a Marker>,
}

/// Whether the person may resolve this unit by taking one side's copy.
pub fn resolvable(r: Resolvable<'_>) -> bool {
    matches!(r.table, super::Decision::Conflict { .. })
        && r.held == r.table
        && r.leaf_marker.is_none_or(|m| m.rel != r.rel)
        && r.home_marker.is_none_or(|m| m.rel != r.rel)
        && !r.leaf.is_empty()
        && !r.home.is_empty()
}

#[cfg(test)]
#[path = "resolve_tests.rs"]
mod tests;
