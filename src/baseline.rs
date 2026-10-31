//! Baselines: the state a leaf and home last agreed on, per unit.
//!
//! **v0.1 reads baselines and never writes one.** None exist yet, so every
//! unit takes the table's no-baseline rows; the reader and the format land now
//! so the decision table is exercised whole and v0.2 only adds the writer.
//!
//! A baseline records both sides' hashes as each side computed them, rather
//! than one agreed hash. The hash is designed to be machine-independent, but
//! comparing each side only with its own past makes the table correct even
//! where it is not (a stat-only difference, a filesystem that rounds mtimes).

use crate::record::{Record, Store};
use crate::state::UnitState;
use crate::units::UnitKind;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Baseline {
    pub version: u32,
    pub rel: String,
    pub kind: UnitKind,
    pub leaf: UnitState,
    pub home: UnitState,
    pub at: u64,
}

impl Record for Baseline {
    fn rel(&self) -> &str {
        &self.rel
    }
    fn version(&self) -> u32 {
        self.version
    }
}

/// A leaf's baselines: `state/baselines/`.
pub fn store(state_dir: &std::path::Path) -> Store {
    Store::at(PathBuf::from(state_dir).join("baselines"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::RECORD_VERSION;

    fn b(rel: &str) -> Baseline {
        Baseline {
            version: RECORD_VERSION,
            rel: rel.into(),
            kind: UnitKind::Repo,
            leaf: UnitState::Present("aa".into()),
            home: UnitState::Empty,
            at: 7,
        }
    }

    #[test]
    fn missing_store_reads_as_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        assert_eq!(s.get::<Baseline>("work/x").unwrap(), None);
        assert!(s.all::<Baseline>().unwrap().is_empty());
    }

    #[test]
    fn round_trip_and_listing_sorted() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        s.put(&b("work/z")).unwrap();
        s.put(&b("work/a/b c")).unwrap();
        assert_eq!(s.get::<Baseline>("work/z").unwrap(), Some(b("work/z")));
        let all: Vec<_> = s
            .all::<Baseline>()
            .unwrap()
            .into_iter()
            .map(|x| x.rel)
            .collect();
        assert_eq!(all, vec!["work/a/b c", "work/z"]);
    }

    #[test]
    fn unknown_version_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        let mut future = b("work/x");
        future.version = 99;
        s.put(&future).unwrap();
        let err = s.get::<Baseline>("work/x").unwrap_err().to_string();
        assert!(err.contains("version 99"), "{err}");
    }

    #[test]
    fn corrupt_record_is_an_error_not_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        s.put(&b("work/x")).unwrap();
        let file = std::fs::read_dir(tmp.path().join("baselines"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        std::fs::write(file.path(), "{not json").unwrap();
        assert!(s.get::<Baseline>("work/x").is_err());
        assert!(s.all::<Baseline>().is_err());
    }
}
