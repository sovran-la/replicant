//! After a unit's rsync: the baseline it leaves, an adoption (no rsync at
//! all), and what the report says it did.

use super::super::AppliedUnit;
use super::expect_done;
use crate::baseline::{self, Baseline};
use crate::daemon::wire::{HomeReply, HomeRequest};
use crate::decide::Decision;
use crate::marker::Direction;
use crate::plan::lease::Watch;
use crate::plan::{HomeLink, LeafCtx, Planned, PlannedUnit, diff};
use crate::record::RECORD_VERSION;
use crate::rsync::Ran;
use crate::{Error, Result, paths, units};

/// The baseline after a transfer: the source as planned, the target hashed
/// again now.
pub(super) fn after(
    link: &dyn HomeLink,
    ctx: LeafCtx<'_>,
    p: &Planned<'_>,
    u: &PlannedUnit,
    direction: Direction,
    lease: u64,
) -> Result<Baseline> {
    let within: Vec<String> = p
        .union
        .iter()
        .filter(|r| paths::is_within(r, &u.rel))
        .cloned()
        .collect();
    let (leaf, home) = match direction {
        Direction::Pull => {
            let spec = diff::spec_for(&p.snap.specs, &u.rel).ok_or_else(|| {
                Error::Refused(format!(
                    "{} is not under any subscription home serves",
                    u.rel
                ))
            })?;
            let sub_abs = ctx.cfg.local_path(&spec.rel, ctx.home_dir);
            let now = units::rehash(spec, &sub_abs, &u.rel, u.kind, &within)?;
            (now.state, u.home.clone())
        }
        Direction::Push => {
            let reply = link.call(HomeRequest::Rehash {
                lease,
                rel: u.rel.clone(),
                kind: u.kind,
                union: within,
            })?;
            let HomeReply::Rehashed { state } = reply else {
                return Err(Error::Remote(format!(
                    "unexpected reply to rehash: {reply:?}"
                )));
            };
            (u.leaf.clone(), state)
        }
    };
    Ok(Baseline {
        version: RECORD_VERSION,
        rel: u.rel.clone(),
        kind: u.kind,
        leaf,
        home,
        at: crate::now_secs(),
    })
}

/// Both sides already hold the same thing: that is the baseline. Or, with
/// `forget`, neither holds anything any more: the baseline goes.
pub(super) fn adopt(
    link: &dyn HomeLink,
    ctx: LeafCtx<'_>,
    watch: &Watch,
    u: &PlannedUnit,
    forget: bool,
) -> Result<AppliedUnit> {
    watch.check()?;
    let b = (!forget).then(|| Baseline {
        version: RECORD_VERSION,
        rel: u.rel.clone(),
        kind: u.kind,
        leaf: u.leaf.clone(),
        home: u.home.clone(),
        at: crate::now_secs(),
    });
    expect_done(
        link.call(HomeRequest::Agree {
            lease: watch.id(),
            rel: u.rel.clone(),
            baseline: b.clone(),
        })?,
        "agree",
    )?;
    let store = baseline::store(ctx.state_dir);
    match &b {
        Some(b) => store.put(b)?,
        None => store.delete(&u.rel)?,
    }
    let why = match &u.decision {
        Decision::Nothing { why } => why.clone(),
        d => d.label(),
    };
    let (action, detail) = match forget {
        false => ("adopt", format!("{why}; recorded as the baseline")),
        true => (
            "forget",
            format!("{why}; gone on both sides, baseline dropped"),
        ),
    };
    expect_done(
        link.call(HomeRequest::Note {
            event: "adopt".into(),
            rel: u.rel.clone(),
            detail,
        })?,
        "note",
    )?;
    Ok(AppliedUnit {
        rel: u.rel.clone(),
        kind: u.kind,
        action: action.into(),
        resumed: false,
        changes: vec![],
        files_changed: 0,
        deleted: 0,
    })
}

pub(super) fn applied(u: &PlannedUnit, action: &str, resumed: bool, ran: &Ran) -> AppliedUnit {
    let d = diff::summarize(&ran.itemized);
    AppliedUnit {
        rel: u.rel.clone(),
        kind: u.kind,
        action: action.into(),
        resumed,
        changes: d.changes,
        files_changed: d.files_changed,
        deleted: d.deleted,
    }
}
