//! What stops `sync` before it asks to apply, and whether there is
//! anything to apply at all.

use crate::decide::Decision;
use crate::guard::Guard;
use crate::plan::Plan;
use crate::plan::apply::confirms;

/// Whether anything but a deletion awaiting confirmation, or a conflict
/// still to be asked about, needs a human. A resume's guardrail 6 is waived
/// by a `--confirm-delete` naming its unit; a conflict the person may
/// resolve blocks only where nobody can be asked (`unasked`), and not once
/// they skipped it (`skip`).
pub fn blockers(
    p: &Plan,
    confirm: &[String],
    home_dir: &std::path::Path,
    skip: &[String],
    unasked: bool,
) -> bool {
    !p.refused.is_empty()
        || p.units.iter().any(|u| {
            let confirmed = confirm.iter().any(|c| confirms(c, u, home_dir));
            let asked = u.resolvable && (!unasked || skip.contains(&u.rel));
            u.refusals
                .iter()
                .any(|r| !(confirmed && r.guard == Guard::ChangedSinceInterrupted))
                || (u.decision.is_blocking()
                    && !matches!(u.decision, Decision::DeletedOn { .. })
                    && !(asked && matches!(u.decision, Decision::Conflict { .. })))
        })
}

/// Whether applying `p` would do anything but leave the skipped conflicts.
pub fn applies(p: &Plan, skip: &[String]) -> bool {
    !p.refused.is_empty()
        || p.units
            .iter()
            .any(|u| u.decision.applies() && !skip.contains(&u.rel))
}
