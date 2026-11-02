//! The direction table, per unit. PLAN.md, "One command":
//!
//! ```text
//! no baseline, target empty   → pull (or push, whichever side has content)
//! no baseline, both populated → conflict; `--adopt home|leaf` is the bulk answer
//! home same,    leaf same     → nothing to do
//! home changed, leaf same     → pull
//! home same,    leaf changed  → push
//! both changed                → conflict
//! interrupted marker present  → resume that direction; never a conflict
//! ```
//!
//! And, from PLAN rev 5 (review F4):
//!
//! ```text
//! unit vanished on one side since baseline → "deleted on X"; never a plain
//!                                            pull/push, explicit confirmation
//! ```
//!
//! "Vanished" is narrow: Present at baseline, Empty now, and the other side
//! unchanged and still populated. A unit that vanished on one side and changed
//! on the other is still a conflict. Everything becoming Empty is far more
//! often a renamed directory or an unmounted volume than a real delete, and
//! as an ordinary change it would send a whole-unit `rsync --delete`.
//!
//! And, for a baseline home no longer serves (review F5, [`decide_unserved`]):
//!
//! ```text
//! unsubscribed baseline → remove from the leaf, only if the host explicitly
//!                         unsubscribed it AND leaf == baseline.leaf (and no
//!                         marker covers it); otherwise conflict
//! ```
//!
//! Plus three rows the plan implies but does not tabulate:
//!
//! - **both empty, no baseline** → nothing (there is nothing to move);
//! - **no baseline, both populated, identical hashes** → nothing. Calling two
//!   byte-identical trees a conflict would make every path of the Syncthing
//!   cutover a prompt; v0.2 adopts the shared state as the first baseline;
//! - **home marker from another leaf** → quarantined (PLAN.md, "Interrupted
//!   sync": pulls of that path by any other leaf are refused).
//!
//! Marker rows come first: an interrupted sync is never a conflict.

use crate::baseline::Baseline;
use crate::marker::{Direction, Marker};
use crate::state::UnitState;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "decision")]
pub enum Decision {
    Nothing {
        why: String,
    },
    Pull,
    Push,
    Conflict {
        why: String,
    },
    /// Rerun an interrupted rsync in its original direction.
    Resume {
        direction: Direction,
    },
    /// Home holds another leaf's dead push of this path.
    Quarantined {
        by: String,
        since: u64,
    },
    /// Explicitly unsubscribed, and a baseline says this leaf has it,
    /// unchanged: the next sync deletes the leaf's copy (through guardrail 1).
    /// See [`decide_unserved`] for when it is not.
    Remove,
    /// Present at baseline, now empty on `host`, and unchanged on the other
    /// side. Never a plain pull or push: carrying it out removes the whole
    /// unit on the other side, so it needs explicit confirmation, and has no
    /// [`Decision::direction`].
    DeletedOn {
        host: String,
    },
}

impl Decision {
    /// The direction rsync would run, if any.
    pub fn direction(&self) -> Option<Direction> {
        match self {
            Decision::Pull => Some(Direction::Pull),
            Decision::Push => Some(Direction::Push),
            Decision::Resume { direction } => Some(*direction),
            _ => None,
        }
    }

    /// Whether this unit needs a human: conflicts, quarantines, and deletions
    /// awaiting confirmation.
    pub fn is_blocking(&self) -> bool {
        matches!(
            self,
            Decision::Conflict { .. } | Decision::Quarantined { .. } | Decision::DeletedOn { .. }
        )
    }

    pub fn label(&self) -> String {
        match self {
            Decision::Nothing { why } => format!("nothing ({why})"),
            Decision::Pull => "pull".into(),
            Decision::Push => "push".into(),
            Decision::Conflict { why } => format!("conflict ({why})"),
            Decision::Resume { direction } => format!("resume {direction}"),
            Decision::Quarantined { by, .. } => format!("quarantined (dead push by {by})"),
            Decision::Remove => "remove (unsubscribed)".into(),
            Decision::DeletedOn { host } => format!("deleted on {host}"),
        }
    }
}

/// What one unit's decision depends on.
#[derive(Debug, Clone, Copy)]
pub struct Inputs<'a> {
    /// This leaf's fleet name.
    pub me: &'a str,
    /// Home's fleet name.
    pub home_name: &'a str,
    pub leaf: &'a UnitState,
    pub home: &'a UnitState,
    pub baseline: Option<&'a Baseline>,
    /// A marker on this leaf covering the unit (a dead pull).
    pub leaf_marker: Option<&'a Marker>,
    /// A marker on home covering the unit (a dead push).
    pub home_marker: Option<&'a Marker>,
}

pub fn decide(i: Inputs<'_>) -> Decision {
    if let Some(m) = i.home_marker.filter(|m| m.host != i.me) {
        return Decision::Quarantined {
            by: m.host.clone(),
            since: m.started_at,
        };
    }
    if let Some(m) = i.leaf_marker {
        return Decision::Resume {
            direction: m.direction,
        };
    }
    if let Some(m) = i.home_marker {
        return Decision::Resume {
            direction: m.direction,
        };
    }

    let Some(b) = i.baseline else {
        return match (i.leaf.is_empty(), i.home.is_empty()) {
            (true, true) => Decision::Nothing {
                why: "both empty".into(),
            },
            (true, false) => Decision::Pull,
            (false, true) => Decision::Push,
            (false, false) if i.leaf == i.home => Decision::Nothing {
                why: "identical".into(),
            },
            (false, false) => Decision::Conflict {
                why: "no baseline, both populated".into(),
            },
        };
    };

    let home_changed = *i.home != b.home;
    let leaf_changed = *i.leaf != b.leaf;
    match (home_changed, leaf_changed) {
        (false, false) => Decision::Nothing {
            why: "unchanged".into(),
        },
        // Changed means it was Present at baseline; the other side still
        // holds it. Carved out of the pull and push rows (review F4).
        (true, false) if i.home.is_empty() && !i.leaf.is_empty() => Decision::DeletedOn {
            host: i.home_name.into(),
        },
        (false, true) if i.leaf.is_empty() && !i.home.is_empty() => {
            Decision::DeletedOn { host: i.me.into() }
        }
        (true, false) => Decision::Pull,
        (false, true) => Decision::Push,
        (true, true) if i.leaf == i.home => Decision::Nothing {
            why: "both changed identically".into(),
        },
        (true, true) => Decision::Conflict {
            why: "both changed since baseline".into(),
        },
    }
}

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
    pub rel: &'a str,
    pub reason: UnservedReason,
    /// The leaf's state of the unit now.
    pub leaf: &'a UnitState,
    pub baseline: &'a Baseline,
    /// Files the leaf holds in the unit now.
    pub files: u64,
    /// A marker on either side covering the unit.
    pub marker: Option<&'a Marker>,
}

/// PLAN's last row (review F5): `Remove` only when the host explicitly
/// unsubscribed the path, the leaf is unchanged since baseline, and no
/// interrupted sync is marked over it. Anything else is a blocking conflict,
/// never a delete: a cap narrowed or a root mistyped is not an unsubscribe,
/// and deleting unpushed loose work is exactly what guardrail 1 cannot see.
pub fn decide_unserved(i: UnservedInputs<'_>) -> Decision {
    let changed = *i.leaf != i.baseline.leaf;
    if i.reason == UnservedReason::Unsubscribed && !changed && i.marker.is_none() {
        return Decision::Remove;
    }
    let files = if i.files == 1 { "file" } else { "files" };
    let since = if changed { "changed" } else { "unchanged" };
    let marked = i
        .marker
        .map(|m| {
            format!(
                "; an interrupted {} by {} is marked here",
                m.direction, m.host
            )
        })
        .unwrap_or_default();
    Decision::Conflict {
        why: format!(
            "home no longer serves {} ({}); {} {files} here, {since} since baseline{marked}; \
             re-subscribe, or unsubscribe to remove",
            i.rel, i.reason, i.files
        ),
    }
}

#[cfg(test)]
#[path = "decide_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "decide_unserved_tests.rs"]
mod unserved_tests;
