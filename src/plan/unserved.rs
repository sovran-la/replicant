//! Baselines home no longer serves (review F5). The leaf hashes its copy of
//! each the way the baseline was hashed, with the excludes and carve-outs
//! home reports for it, and [`decide::decide_unserved`] decides: `Remove`
//! only for an explicit unsubscribe of an unchanged, unmarked unit; an
//! orphan, a conflict or a broken home root otherwise. A push of the unit
//! home rolled back holds a `Remove` until accepted, as `decide_all` holds
//! a pull.

use super::apply::accepts_rollback;
use super::{LeafCtx, PlannedUnit};
use crate::baseline::Baseline;
use crate::daemon::wire::HomeSnapshot;
use crate::decide::{self, UnservedInputs, UnservedReason};
use crate::marker::{self, Marker};
use crate::state::UnitState;
use crate::units::{self, Discovered, SubSpec, UnitKind};
use crate::{Result, paths};

/// The planned units, and the leaf repos found under them: a loose unit's
/// rsync must exclude those, or removing it would delete them too.
pub(super) struct Unserved {
    pub units: Vec<PlannedUnit>,
    pub repos: Vec<String>,
}

/// One planned unit per baseline outside every spec home serves.
pub(super) fn plan(
    ctx: LeafCtx<'_>,
    home_name: &str,
    snap: &HomeSnapshot,
    baselines: &[Baseline],
    leaf_markers: &[Marker],
) -> Result<Unserved> {
    let out: Vec<&Baseline> = baselines
        .iter()
        .filter(|b| !snap.specs.iter().any(|s| s.covers(&b.rel)))
        .collect();
    if out.is_empty() {
        return Ok(Unserved {
            units: vec![],
            repos: vec![],
        });
    }
    let told = |rel: &str| snap.unserved.iter().find(|u| u.spec.rel == rel);
    // A loose baseline is a former subscription: walk it once, and every
    // repo baseline inside it with it.
    let loose: Vec<&str> = out
        .iter()
        .filter(|b| b.kind == UnitKind::Loose)
        .map(|b| b.rel.as_str())
        .collect();
    let specs: Vec<SubSpec> = out
        .iter()
        .filter(|b| b.kind == UnitKind::Loose || !loose.iter().any(|l| paths::is_within(&b.rel, l)))
        .map(|b| match told(&b.rel) {
            Some(u) => u.spec.clone(),
            // An older home, or a path home never heard of: no excludes. A
            // hash that differs for it reads as changed, which is a conflict.
            None => SubSpec {
                rel: b.rel.clone(),
                exclude: vec![],
                except: vec![],
                root: String::new(),
            },
        })
        .collect();
    let local = |rel: &str| ctx.cfg.local_path(rel, ctx.home_dir);
    let found = Discovered::run(&specs, &local)?;
    let mut union = found.repos();
    union.extend(
        out.iter()
            .filter(|b| b.kind == UnitKind::Repo)
            .map(|b| b.rel.clone()),
    );
    union.sort();
    union.dedup();
    let leaf_units = units::snapshot(&found, &union, false)?;

    let empty = UnitState::Empty;
    let units = out
        .into_iter()
        .map(|b| {
            let u = leaf_units
                .iter()
                .find(|u| u.rel == b.rel && u.kind == b.kind);
            let leaf = u.map_or(&empty, |u| &u.state);
            let files = u.map_or(0, |u| u.files);
            let path = local(&b.rel);
            let told = told(&b.rel);
            let table = decide::decide_unserved(UnservedInputs {
                home_name,
                rel: &b.rel,
                path: &path.display().to_string(),
                reason: told.map_or(UnservedReason::NotServed, |u| u.reason),
                detail: told.map_or("", |u| u.detail.as_str()),
                leaf,
                baseline: b,
                files,
                marker: marker::covering(leaf_markers, &b.rel)
                    .or_else(|| marker::covering(&snap.markers, &b.rel)),
                me: ctx.me,
            });
            // A rolled-back push holds a removal as it holds a pull
            // (re-review R2): home no longer holds what the leaf pushed.
            let decision = decide::hold_rolled_back(
                table,
                decide::Held {
                    rel: &b.rel,
                    home_name,
                    leaf,
                    home: &empty,
                    leaf_path: &path,
                    home_path: &snap.home_dir.join(&b.rel),
                    rolled_back: snap.rolled_back.iter().find(|r| r.unit == b.rel),
                    accepted: accepts_rollback(ctx, &b.rel, &path),
                },
            );
            PlannedUnit {
                rel: b.rel.clone(),
                kind: b.kind,
                decision,
                leaf: leaf.clone(),
                home: UnitState::Empty,
                leaf_head: u.and_then(|u| u.head.clone()),
                home_head: None,
                leaf_path: path,
                home_path: snap.home_dir.join(&b.rel),
                diff: None,
                refusals: vec![],
                leaf_files: files,
                home_files: 0,
            }
        })
        .collect();
    Ok(Unserved {
        units,
        repos: union,
    })
}
