//! In-progress markers: the record that a transfer into this side started and
//! has not been seen to finish. PLAN.md, "Interrupted sync".
//!
//! The *target* side writes one before the transfer starts and clears it in the same
//! step that writes the baseline. A marker on a leaf is a dead pull, private
//! to that leaf, resumed by its next sync. A marker on home is a dead push and
//! **quarantines** the path: the pushing leaf resumes it; everyone else's pulls
//! of it are refused.
//!
//! `sync`'s apply writes them (`plan/apply_unit.rs` on the leaf,
//! `daemon/home_apply.rs` on home).

use crate::paths;
use crate::record::{Record, Store};
use crate::state::UnitState;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Pull,
    Push,
}

impl std::fmt::Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Direction::Pull => "pull",
            Direction::Push => "push",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub version: u32,
    /// The path the interrupted transfer covered.
    pub rel: String,
    pub direction: Direction,
    /// The leaf that ran the sync.
    pub host: String,
    /// The source side's state when it started.
    pub source_state: UnitState,
    pub started_at: u64,
    /// A push's id on home (`<host>/<YYYYMMDDTHHMMSSZ>`): its backup and
    /// manifest. Absent on a pull, and on a marker from before backups.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<String>,
}

impl Record for Marker {
    fn rel(&self) -> &str {
        &self.rel
    }
    fn version(&self) -> u32 {
        self.version
    }
}

impl Marker {
    /// Whether this marker fences `unit`: one lies within the other.
    pub fn covers(&self, unit: &str) -> bool {
        paths::is_within(unit, &self.rel) || paths::is_within(&self.rel, unit)
    }
}

/// This side's markers: `state/markers/`.
pub fn store(state_dir: &Path) -> Store {
    Store::at(PathBuf::from(state_dir).join("markers"))
}

/// The marker fencing `unit`, if any. The first one found wins; two markers
/// over one unit would mean two syncs ran into it at once, which the queue
/// exists to make impossible.
pub fn covering<'m>(markers: &'m [Marker], unit: &str) -> Option<&'m Marker> {
    markers.iter().find(|m| m.covers(unit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::RECORD_VERSION;

    fn m(rel: &str) -> Marker {
        Marker {
            version: RECORD_VERSION,
            rel: rel.into(),
            direction: Direction::Push,
            host: "birchbook".into(),
            source_state: UnitState::Present("ab".into()),
            started_at: 1,
            push: None,
        }
    }

    #[test]
    fn covers_ancestors_and_descendants_not_siblings() {
        let mk = m("work/tundra");
        assert!(mk.covers("work/tundra"));
        assert!(mk.covers("work/tundra/beacon"));
        assert!(mk.covers("work"), "a loose unit containing the marked path");
        assert!(!mk.covers("work/tundra2"));
        assert!(!mk.covers("work/sequoia"));
    }

    #[test]
    fn store_round_trip_and_covering() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        s.put(&m("work/a")).unwrap();
        let all: Vec<Marker> = s.all().unwrap();
        assert_eq!(all, vec![m("work/a")]);
        assert_eq!(covering(&all, "work/a/x"), Some(&all[0]));
        assert_eq!(covering(&all, "work/b"), None);
    }

    #[test]
    fn direction_display() {
        assert_eq!(Direction::Pull.to_string(), "pull");
        assert_eq!(Direction::Push.to_string(), "push");
    }
}
