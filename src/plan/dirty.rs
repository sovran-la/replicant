//! Guardrail 1 in the plan: which target's uncommitted work a unit's
//! transfer is held to (`guard::dirty_target`).
//!
//! - A plain push or pull onto a target still at its baseline moves dirt
//!   both sides agreed on at the last sync (rev 21): passed as `agreed`.
//! - A resolved conflict (rev 22) overwrites the target's work because the
//!   person said so, and keeps a backup of it instead: not held at all. Nor
//!   is the resume of an interrupted resolved pull, whose marker names the
//!   backup it keeps writing into; guardrail 6 still holds it to whatever
//!   the leaf wrote after the interruption.
//! - Everything else (a resume, a removal, a confirmed deletion) is held
//!   whole.

use super::PlannedUnit;
use crate::baseline::Baseline;
use crate::changes::Itemized;
use crate::decide::Decision;
use crate::guard::{self, Refusal};
use crate::marker::{Direction, Marker};
use crate::units::SideUnit;

pub(super) fn guard(
    p: &PlannedUnit,
    direction: Direction,
    target: Option<&SideUnit>,
    it: &Itemized,
    baselines: &[Baseline],
    leaf_markers: &[Marker],
) -> Option<Refusal> {
    if resolved(p, direction, leaf_markers) {
        return None;
    }
    let agreed = match p.decision {
        Decision::Push | Decision::Pull => baselines
            .iter()
            .find(|b| b.rel == p.rel && b.kind == p.kind)
            .map(|b| match direction {
                Direction::Push => &b.home,
                Direction::Pull => &b.leaf,
            }),
        _ => None,
    };
    target.and_then(|t| guard::dirty_target(t, it, agreed))
}

/// A resolved conflict, or the resume of one interrupted while it pulled.
fn resolved(p: &PlannedUnit, direction: Direction, leaf_markers: &[Marker]) -> bool {
    match p.decision {
        Decision::Resolved { .. } => true,
        Decision::Resume { .. } => {
            direction == Direction::Pull
                && leaf_markers
                    .iter()
                    .any(|m| m.rel == p.rel && m.backup.is_some())
        }
        _ => false,
    }
}
