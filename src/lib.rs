//! replicant: explicit, home-base-mediated rsync for keeping `~/work` (and a
//! few config dirs) identical across machines.
//!
//! The design is docs/PLAN.md. It subscribes, approves, reports status,
//! computes `sync --dry-run`, and (v0.2) applies `sync`: synced paths are
//! written only by rsync, and only after the whole plan may go
//! (`plan::apply`).

pub mod baseline;
pub mod cli;
pub mod config;
pub mod daemon;
pub mod decide;
pub mod error;
pub mod exclude;
pub mod git;
pub mod guard;
pub mod marker;
pub mod paths;
pub mod plan;
pub mod record;
pub mod repos;
pub mod rsync;
pub mod state;
pub mod subscribe;
pub mod timefmt;
pub mod units;

#[cfg(test)]
pub(crate) mod testutil;

#[cfg(test)]
mod write_set_tests;

pub use error::{Error, Result};

/// Seconds since the Unix epoch.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
