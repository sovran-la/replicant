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
pub mod polled;
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

/// The temporary name for a file called `name`, in the same directory.
pub fn temp_name(name: &str) -> String {
    /// Longest file name most filesystems take.
    const NAME_MAX: usize = 255;
    match TEMP_PREFIX.len() + name.len() <= NAME_MAX {
        true => format!("{TEMP_PREFIX}{name}"),
        false => {
            use sha2::Digest;
            let h = sha2::Sha256::digest(name.as_bytes());
            let hex: String = h[..12].iter().map(|b| format!("{b:02x}")).collect();
            format!("{TEMP_PREFIX}{hex}")
        }
    }
}

/// Files smaller than this travel many to a [`frame::Kind::Batch`].
pub const SMALL: u64 = 64 << 10;

/// A batch closes at this many bytes of files.
pub const BATCH_BYTES: u64 = 1 << 20;

/// A large file streams in chunks of this many bytes.
pub const CHUNK: usize = 1 << 20;

/// How long a sender waits for an ack before it calls the connection dead.
pub const ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// fsync(2): a file's bytes and metadata handed to the device, before a
/// rename makes them visible (and a directory's names, after renames into
/// it). Not `File::sync_all`, which on macOS is `F_FULLFSYNC`, a drive-cache
/// flush per file: ~4 ms each, which made 10,000 small files take 40 s
/// instead of 1.6 s. POSIX `fsync` is what Linux's `sync_all` is anyway.
pub fn fsync(f: &std::fs::File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: the descriptor is open for the life of `f`.
    match unsafe { libc::fsync(f.as_raw_fd()) } {
        0 => Ok(()),
        _ => Err(std::io::Error::last_os_error()),
    }
}

/// Whether a file name is one of the receiver's temporary names.
pub fn is_temp(name: &str) -> bool {
    name.starts_with(TEMP_PREFIX)
}
