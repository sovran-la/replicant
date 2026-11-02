//! What a leaf tells home when a plan is done: for the fleet view and the
//! journal.

use super::{HomeLink, LeafCtx, Plan};
use crate::daemon::wire::{HomeRequest, ReportedUnit};
use crate::marker;

/// Tell home what was decided (and, for a sync, whether it finished), with
/// this leaf's open markers, for the fleet view and the journal. The work
/// is done either way; only home's line is missing if this fails.
pub(crate) fn report(
    link: &dyn HomeLink,
    ctx: LeafCtx<'_>,
    event: &str,
    plan: &Plan,
    summary: String,
    sync_ok: Option<bool>,
) {
    let markers = marker::store(ctx.state_dir).all().unwrap_or_default();
    let reported = plan
        .units
        .iter()
        .map(|u| ReportedUnit {
            rel: u.rel.clone(),
            decision: u.decision.clone(),
        })
        .collect();
    if let Err(e) = link.call(HomeRequest::Report {
        event: event.into(),
        units: reported,
        summary,
        markers,
        sync_ok,
    }) {
        eprintln!("replicant: the {event} finished, but home did not journal it: {e}");
    }
}
