//! Which rsync this machine runs, because it decides how home's path must be
//! written on the command line.
//!
//! The remote half of `host:path` reaches home through ssh, and ssh hands the
//! words it is given to home's shell. rsync ≥ 3.2.4 escapes the path itself
//! before that; openrsync (macOS's `/usr/bin/rsync`) passes it as is, so a
//! space splits it and a `$(...)` runs on home. For openrsync replicant
//! single-quotes the path; for rsync it must not, or rsync escapes it twice.

use crate::{Error, Result};
use std::process::Command;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Client {
    /// OpenBSD's rsync, macOS's `/usr/bin/rsync` ("openrsync: protocol version 29").
    OpenRsync,
    /// Samba rsync (3.x), which protects remote arguments itself.
    Rsync,
}

impl Client {
    /// Classify `rsync --version` output.
    pub fn from_version(text: &str) -> Self {
        if text.contains("openrsync") {
            Client::OpenRsync
        } else {
            Client::Rsync
        }
    }

    /// The `rsync` on `PATH`, asked once per process.
    pub fn detect() -> Result<Self> {
        static FOUND: OnceLock<Client> = OnceLock::new();
        if let Some(c) = FOUND.get() {
            return Ok(*c);
        }
        let out = Command::new("rsync")
            .arg("--version")
            .output()
            .map_err(|e| Error::Rsync(format!("could not run rsync --version: {e}")))?;
        // openrsync prints its banner on stdout; read both to be sure.
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok(*FOUND.get_or_init(|| Client::from_version(&text)))
    }

    /// Home's path as the remote half of `host:path` for this client.
    pub fn remote_path(self, path: &str) -> String {
        match self {
            Client::OpenRsync => shell_quote(path),
            Client::Rsync => path.to_string(),
        }
    }
}

/// One POSIX shell word: `'...'`, with each `'` written `'\''`.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}
