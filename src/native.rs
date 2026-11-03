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

pub mod diff;
pub mod manifest;

pub use diff::{ChangeList, diff, itemize};
pub use manifest::{Entry, EntryKind, Manifest};

/// The prefix of a file being received. Never part of a manifest, and
/// removed by the receiver at the start of the next transfer of its unit.
pub const TEMP_PREFIX: &str = ".replicant-tmp-";

/// Whether a file name is one of the receiver's temporary names.
pub fn is_temp(name: &str) -> bool {
    name.starts_with(TEMP_PREFIX)
}
