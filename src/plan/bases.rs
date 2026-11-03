//! Review F4: a subscription base missing on its own side is an error, not an
//! empty unit, unless nothing under it was ever synced.
//!
//! "Everything became empty" is far more often a renamed `~/work2`, an
//! unmounted volume or an `--as` pointed at nothing than a real delete. Read
//! as empty, every unit under it would plan a whole-unit delete.
//! A base may be absent only when no baseline lies under it: the first pull
//! onto a fresh leaf, or the first push of something home has never had.

use crate::baseline::Baseline;
use crate::{Error, Result, paths};
use std::path::PathBuf;

/// `missing` is each absent subscription's (rel, path on `host`).
pub(super) fn check(
    missing: &[(String, PathBuf)],
    baselines: &[Baseline],
    host: &str,
) -> Result<()> {
    let errors: Vec<String> = missing
        .iter()
        .filter_map(|(rel, path)| {
            let synced = baselines.iter().find(|b| paths::is_within(&b.rel, rel))?;
            Some(format!(
                "subscription base {} does not exist on {host}, but {} was synced there \
                 (a baseline exists); restore it or change the subscription",
                path.display(),
                synced.rel
            ))
        })
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(Error::Missing(errors.join("; ")))
    }
}
