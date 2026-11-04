//! Which of apply's steps a planned unit takes.

use crate::baseline::Baseline;
use crate::decide::Decision;
use crate::marker::Direction;
use crate::plan::xfer::PlannedXfer;
use crate::plan::{LeafCtx, PlannedUnit};

/// What to do with one planned unit.
pub(super) enum Step<'p> {
    /// Run its transfer. `deleting`: the source is gone, so afterwards the
    /// baseline is removed rather than written.
    Transfer {
        t: &'p PlannedXfer,
        action: &'static str,
        resumed: bool,
        deleting: bool,
    },
    /// Both sides already agree: record it as the baseline.
    Adopt,
    /// Gone on both sides: the baseline goes too.
    Forget,
    Skip,
}

pub(super) fn step<'p>(
    ctx: LeafCtx<'_>,
    u: &PlannedUnit,
    t: Option<&'p PlannedXfer>,
    b: Option<&Baseline>,
) -> Step<'p> {
    let Some(t) = t else {
        let agreed = b.is_some_and(|b| b.leaf == u.leaf && b.home == u.home);
        return match &u.decision {
            Decision::Nothing { .. } if !u.leaf.is_empty() && !u.home.is_empty() && !agreed => {
                Step::Adopt
            }
            Decision::Nothing { .. } if u.leaf.is_empty() && u.home.is_empty() && b.is_some() => {
                Step::Forget
            }
            _ => Step::Skip,
        };
    };
    let (action, resumed) = match &u.decision {
        Decision::Pull => ("pull", false),
        Decision::Push => ("push", false),
        Decision::Remove => ("remove", false),
        Decision::DeletedOn { host } if host == ctx.me => ("delete on home", false),
        Decision::DeletedOn { .. } => ("delete here", false),
        Decision::Resume { direction } => (
            match direction {
                Direction::Pull => "pull",
                Direction::Push => "push",
            },
            true,
        ),
        _ => return Step::Skip,
    };
    Step::Transfer {
        t,
        action,
        resumed,
        deleting: t.xfer.source_absent,
    }
}
