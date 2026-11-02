//! A push home rolled back holds the host that pushed it (review A2).
//!
//! Rollback restores home and leaves baselines alone, so the pushing leaf's
//! next sync sees "home changed, leaf same" and would pull home's restored
//! copy over the very work it pushed, which then survives only in home's
//! backup until the sweep. So until that host says so, the unit is a
//! blocking conflict for it, wherever the table would write the leaf:
//!
//! ```text
//! rolled back push of mine, leaf != home:
//!   pull, resume pull, deleted on home,
//!   remove (unsubscribed)              → conflict "home rolled back your push"
//!   conflict                           → the same conflict, saying both
//!   anything else (a push, nothing)    → as the table says; the leaf is not
//!                                        written
//! rolled back, leaf == home            → as the table says (adopted, which
//!                                        settles the rollback)
//! accepted (`--accept-rollback <path>`) → as the table says
//! ```
//!
//! The unserved rows (`decide_unserved`) go through it too (re-review R2):
//! a `Remove` deletes the leaf's copy on the premise that home holds what
//! the leaf holds, which a rollback breaks.
//!
//! Home stops reporting it once the host and home agree on the unit again
//! (a finished pull, an adoption, a later push): `Agree` with a baseline
//! settles it. An `Agree` without one (a removal, a deletion, a forget)
//! leaves the leaf's pushed work only in home's backup, so it settles only
//! when the host accepted the rollback for that unit.

use super::Decision;
use crate::marker::Direction;
use crate::state::UnitState;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A push of this host's that home rolled back, not yet settled: home
/// reports these in [`crate::daemon::wire::HomeSnapshot::rolled_back`],
/// only the asking host's own, the newest per unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolledBackPush {
    /// `<host>/<YYYYMMDDTHHMMSSZ>`.
    pub id: String,
    pub unit: String,
    /// When home rolled it back.
    pub at: u64,
}

/// What a rolled-back push makes of the table's decision `d` for its unit.
#[derive(Debug, Clone, Copy)]
pub struct Held<'a> {
    pub rel: &'a str,
    pub home_name: &'a str,
    pub leaf: &'a UnitState,
    pub home: &'a UnitState,
    pub leaf_path: &'a Path,
    pub home_path: &'a Path,
    pub rolled_back: Option<&'a RolledBackPush>,
    /// The host passed `--accept-rollback` naming this unit.
    pub accepted: bool,
}

pub fn hold_rolled_back(d: Decision, h: Held<'_>) -> Decision {
    let Some(rb) = h.rolled_back.filter(|_| !h.accepted && h.leaf != h.home) else {
        return d;
    };
    let accepting = match d {
        Decision::Remove => "lets the removal delete the leaf's copy",
        _ => "takes home's copy over the leaf's",
    };
    let why = format!(
        "home rolled back your push {} at {}; leaf {}, home {}. `sync --accept-rollback {}` \
         {accepting}",
        rb.id,
        crate::timefmt::iso(rb.at),
        h.leaf_path.display(),
        h.home_path.display(),
        h.rel
    );
    match d {
        Decision::Pull
        | Decision::Remove
        | Decision::Resume {
            direction: Direction::Pull,
        } => Decision::Conflict { why },
        Decision::DeletedOn { ref host } if host == h.home_name => Decision::Conflict { why },
        Decision::Conflict { why: was } => Decision::Conflict {
            why: format!("{why}; and {was}"),
        },
        d => d,
    }
}

#[cfg(test)]
#[path = "rollback_tests.rs"]
mod tests;
