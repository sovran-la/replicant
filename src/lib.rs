//! replicant: explicit, home-base-mediated rsync for keeping `~/work` (and a
//! few config dirs) identical across machines.
//!
//! The design is docs/PLAN.md. v0.1 is read-only: it subscribes, approves,
//! reports status and computes `sync --dry-run`, and never writes a synced
//! path or a baseline.

pub mod config;
pub mod error;
pub mod paths;

pub use error::{Error, Result};

/// Seconds since the Unix epoch.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
