//! The rows for a baseline home no longer serves (review F5, as amended
//! 2026-11-01). The deletion rule is narrow: `Remove` only for an explicit
//! unsubscribe of an unchanged, unmarked unit. Everything else is weighted
//! by why home stopped serving it, which home reports per path.

use super::Decision;
use crate::baseline::Baseline;
use crate::marker::Marker;
use crate::state::UnitState;
use serde::{Deserialize, Serialize};

/// Why home no longer serves a path a leaf holds a baseline for. Home says;
/// a leaf that is not told assumes [`UnservedReason::NotServed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnservedReason {
    /// The host ran `unsubscribe` on it: a removed subscription or a
    /// carve-out. The only reason that can become [`Decision::Remove`].
    Unsubscribed,
    /// Its subscription is outside the host's cap, narrowed since.
    CapNarrowed,
    /// Its subscription is under no root fleet.toml configures: a root
    /// removed, renamed or mistyped.
    RootMissing,
    /// Its subscription lies wholly inside one of its root's excludes
    /// (review P3): an exclude added since.
    Excluded,
    /// None of the above: never subscribed as far as home knows, or dropped
    /// from `paths` by a hand edit.
    NotServed,
}

impl std::fmt::Display for UnservedReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            UnservedReason::Unsubscribed => "unsubscribed",
            UnservedReason::CapNarrowed => "cap narrowed",
            UnservedReason::RootMissing => "no root serves it",
            UnservedReason::Excluded => "excluded by its root",
            UnservedReason::NotServed => "not subscribed",
        })
    }
}

/// What a baseline home no longer serves is decided on.
#[derive(Debug, Clone, Copy)]
pub struct UnservedInputs<'a> {
    /// Home's fleet name, for "fix fleet.toml on <home>".
    pub home_name: &'a str,
    pub rel: &'a str,
    /// The unit's local path, as the user would type it.
    pub path: &'a str,
    pub reason: UnservedReason,
    /// Home's explanation, for [`UnservedReason::RootMissing`].
    pub detail: &'a str,
    /// The leaf's state of the unit now.
    pub leaf: &'a UnitState,
    pub baseline: &'a Baseline,
    /// Files the leaf holds in the unit now.
    pub files: u64,
    /// A marker on either side covering the unit.
    pub marker: Option<&'a Marker>,
}

/// "3 files here".
pub fn files_here(n: u64) -> String {
    format!("{n} {} here", if n == 1 { "file" } else { "files" })
}

/// The row for one baseline outside everything home serves.
///
/// A baseline holds hashes, not manifests, so "changed" is known and how
/// many files changed is not; messages say how many files the leaf holds.
pub fn decide_unserved(i: UnservedInputs<'_>) -> Decision {
    let changed = *i.leaf != i.baseline.leaf;
    if let Some(m) = i.marker {
        let since = if changed { "changed" } else { "unchanged" };
        return Decision::Conflict {
            why: format!(
                "home no longer serves {} ({}); an interrupted {} by {} is marked here; {}, \
                 {since} since baseline; finish or roll back that sync first",
                i.rel,
                i.reason,
                m.direction,
                m.host,
                files_here(i.files)
            ),
        };
    }
    match i.reason {
        UnservedReason::RootMissing => Decision::HomeRootMissing {
            why: format!(
                "home's root for {} is missing or renamed ({}); fix fleet.toml on {}",
                i.rel, i.detail, i.home_name
            ),
        },
        UnservedReason::Unsubscribed if !changed => Decision::Remove,
        UnservedReason::Unsubscribed => Decision::Conflict {
            why: format!(
                "unsubscribed but changed here since baseline ({}); push first, or \
                 `replicant unsubscribe --force {}`",
                files_here(i.files),
                i.path
            ),
        },
        reason => Decision::Orphaned { reason, changed },
    }
}
