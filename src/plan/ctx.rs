//! Who and where a leaf is, for one plan or sync.

use super::Plane;
use crate::config::LeafConfig;
use std::path::{Path, PathBuf};

/// Who and where this leaf is.
#[derive(Debug, Clone, Copy)]
pub struct LeafCtx<'a> {
    pub me: &'a str,
    pub home_dir: &'a Path,
    pub cfg: &'a LeafConfig,
    pub state_dir: &'a Path,
    /// Which data plane moves the bytes.
    pub plane: Plane,
    /// `--accept-rollback`: units (by unit path or this leaf's path) whose
    /// rolled-back push this host takes home's copy for (review A2).
    pub accept_rollback: &'a [String],
    /// The person's answers to conflicts: each unit takes one side's copy
    /// ([`crate::decide::Resolution`]).
    pub resolve: &'a [crate::decide::Resolution],
}

impl LeafCtx<'_> {
    /// Where this leaf keeps what a resolved pull replaces: beside its
    /// state directory, as [`crate::config::Dirs::backup`] is beside
    /// [`crate::config::Dirs::state`].
    pub fn backup_dir(&self) -> PathBuf {
        self.state_dir.with_file_name("backup")
    }
}
