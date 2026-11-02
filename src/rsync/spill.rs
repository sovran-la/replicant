//! A long exclude list goes to rsync in a file, not on the command line.
//!
//! A repo unit excludes one anchored literal per git-ignored path, on both
//! sides. That is usually a handful (`target/` is one rule), but a tree that
//! ignores `*.pyc` lists every one, and the command line has a limit
//! (`ARG_MAX`, 1 MiB on macOS including the environment). Past [`LIMIT`]
//! bytes the anchored rules move to `--exclude-from=<file>`, a private
//! mkstemp(3) file under the config dir, removed when the run is done.
//!
//! A line of an exclude file means what the same text after `--exclude=`
//! means, to openrsync and to rsync 3.4.1 alike (probed: trailing spaces,
//! `#`, `;`, `+ ` and escaped `*` all survive; `docs/v0.2-prep-notes.md`).
//! Only rules starting with `/` move: a leading `#` or `;` would be read as a
//! comment, and every anchored rule starts with `/`. A rule holding a line
//! break stays on the command line, where it is still one argument. The
//! rules are all excludes, so moving some does not change what matches.

use super::scratch;
use crate::Result;
use std::io::Write;
use std::os::fd::FromRawFd;
use std::path::{Path, PathBuf};

/// Bytes of anchored rules (after `--exclude=`) the command line keeps
/// before they spill.
pub(super) const LIMIT: usize = 32 * 1024;

const FLAG: &str = "--exclude=";

/// An exclude file, removed on drop.
#[derive(Debug)]
pub(super) struct ExcludeFile(PathBuf);

impl ExcludeFile {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ExcludeFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn movable(arg: &str) -> Option<&str> {
    arg.strip_prefix(FLAG)
        .filter(|r| r.starts_with('/') && !r.contains(['\n', '\r']))
}

/// `args` with its anchored exclude rules moved into a file when their
/// arguments pass `limit` bytes; the file, while the run needs it.
pub(super) fn spill(
    args: Vec<String>,
    scratch: &Path,
    limit: usize,
) -> Result<(Vec<String>, Option<ExcludeFile>)> {
    let size: usize = args.iter().filter_map(|a| movable(a)).map(str::len).sum();
    if size <= limit {
        return Ok((args, None));
    }
    let mut bytes = scratch::template(scratch, "exclude-")?;
    // SAFETY: `bytes` is a NUL-terminated, writable buffer that outlives the
    // call; mkstemp rewrites the X's in place and returns an owned fd.
    let fd = unsafe { libc::mkstemp(bytes.as_mut_ptr().cast()) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let file = ExcludeFile(scratch::made(bytes));
    // SAFETY: `fd` was just opened by mkstemp and nothing else owns it.
    let mut w = std::io::BufWriter::new(unsafe { std::fs::File::from_raw_fd(fd) });
    let mut out = Vec::with_capacity(args.len());
    let mut flagged = false;
    for a in args {
        match movable(&a) {
            Some(rule) => {
                writeln!(w, "{rule}")?;
                if !flagged {
                    out.push(format!("--exclude-from={}", file.path().display()));
                    flagged = true;
                }
            }
            None => out.push(a),
        }
    }
    w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    Ok((out, Some(file)))
}

#[cfg(test)]
#[path = "spill_tests.rs"]
mod tests;
