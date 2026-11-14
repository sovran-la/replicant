//! Every unit both sides hold, decided by the direction table.

use super::apply::accepts_rollback;
use super::{LeafCtx, PlannedUnit};
use crate::baseline::Baseline;
use crate::daemon::wire::HomeSnapshot;
use crate::decide::{self, Decision};
use crate::marker::Marker;
use crate::state::UnitState;
use crate::units::SideUnit;

pub(super) fn decide_all(
    ctx: LeafCtx<'_>,
    home_name: &str,
    baselines: &[Baseline],
    snap: &HomeSnapshot,
    leaf_units: &[SideUnit],
    leaf_markers: &[Marker],
) -> Vec<PlannedUnit> {
    let mut out = Vec::new();
    for l in leaf_units {
        let h = snap
            .units
            .iter()
            .find(|h| h.rel == l.rel && h.kind == l.kind);
        let empty = UnitState::Empty;
        let home_state = h.map(|h| &h.state).unwrap_or(&empty);
        let b = baselines.iter().find(|b| b.rel == l.rel);
        let leaf_path = ctx.cfg.local_path(&l.rel, ctx.home_dir);
        let home_path = snap.home_dir.join(&l.rel);
        let leaf_marker = leaf_markers.iter().find(|m| m.rel == l.rel);
        // Another host's marker anywhere over the unit quarantines it;
        // this leaf's own resume only their own unit (`decide` holds
        // that rule too).
        let home_marker = snap
            .markers
            .iter()
            .find(|m| m.host != ctx.me && m.covers(&l.rel))
            .or_else(|| snap.markers.iter().find(|m| m.rel == l.rel));
        let table = decide::decide(decide::Inputs {
            rel: &l.rel,
            me: ctx.me,
            home_name,
            leaf: &l.state,
            home: home_state,
            baseline: b,
            leaf_marker,
            home_marker,
        });
        let held = decide::hold_rolled_back(
            table.clone(),
            decide::Held {
                rel: &l.rel,
                home_name,
                leaf: &l.state,
                home: home_state,
                leaf_path: &leaf_path,
                home_path: &home_path,
                rolled_back: snap.rolled_back.iter().find(|r| r.unit == l.rel),
                accepted: accepts_rollback(ctx, &l.rel, &leaf_path),
            },
        );
        let resolvable = decide::resolvable(decide::Resolvable {
            rel: &l.rel,
            table: &table,
            held: &held,
            leaf: &l.state,
            home: home_state,
            leaf_marker,
            home_marker,
        });
        // The person's answer, for a conflict that may take one.
        let decision = match ctx.resolve.iter().find(|r| r.rel == l.rel) {
            Some(r) if resolvable => Decision::Resolved { take: r.take },
            _ => held,
        };
        out.push(PlannedUnit {
            rel: l.rel.clone(),
            kind: l.kind,
            decision,
            leaf: l.state.clone(),
            home: home_state.clone(),
            leaf_head: l.head.clone(),
            home_head: h.and_then(|h| h.head.clone()),
            leaf_path,
            home_path,
            diff: None,
            refusals: vec![],
            leaf_files: l.files,
            home_files: h.map_or(0, |h| h.files),
            resolvable,
            leaf_dirty: l.dirty.len(),
            home_dirty: h.map_or(0, |h| h.dirty.len()),
        });
    }
    out
}
