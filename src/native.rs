//! The native data plane: the daemons move a unit's bytes themselves.
//!
//! Each side walks the unit into a [`Manifest`] (path, kind, size, mtime to
//! the nanosecond, mode, link target), with the unit's excludes applied as
//! it walks. [`diff`] of the source's against the target's is the change
//! list, and that list *is* the dry run: rendered as [`Itemized`], it is
//! what the plan shows, the guardrails read and the plan digest hashes.
//!
//! A transfer carries the change list out over the daemons' mTLS link:
//! files are written to a temporary name in their directory, given the
//! source's mode and mtime, fsynced and renamed into place, so a target
//! file is always whole. Deletions run only after every file arrived.
//!
//! [`Itemized`]: crate::changes::Itemized

pub mod backup;
pub mod conn;
pub mod diff;
pub mod frame;
pub mod local;
pub mod manifest;
pub mod receive;
pub mod send;
pub mod serve;
mod serve_pull;
pub mod session;
pub mod source;
pub mod transfer;
pub mod workers;

#[cfg(test)]
pub(crate) mod tests_fixture;

#[cfg(test)]
mod tests_faulty;

pub use diff::{ChangeList, diff, itemize};
pub use manifest::{Entry, EntryKind, Manifest};

/// The prefix of a file being received. Never part of a manifest, and
/// removed by the receiver at the start of the next transfer of its unit.
pub const TEMP_PREFIX: &str = ".replicant-tmp-";

/// Files smaller than this travel many to a [`frame::Kind::Batch`].
pub const SMALL: u64 = 64 << 10;

/// A batch closes at this many bytes of files.
pub const BATCH_BYTES: u64 = 1 << 20;

/// A large file streams in chunks of this many bytes.
pub const CHUNK: usize = 1 << 20;

/// How long a sender waits for an ack before it calls the connection dead.
pub const ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Whether a file name is one of the receiver's temporary names.
pub fn is_temp(name: &str) -> bool {
    name.starts_with(TEMP_PREFIX)
}
