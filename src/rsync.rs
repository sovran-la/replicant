//! The data plane: build an rsync command and parse its itemized output.
//!
//! replicant never copies a byte itself. In v0.1 it never runs rsync for real
//! either: [`Transfer::args`] always carries `--dry-run`, and there is no code
//! path that omits it.
//!
//! Output is read with `--itemize-changes --out-format=%i %n`, which both
//! rsync 3 (Linux) and openrsync (macOS's /usr/bin/rsync, protocol 29) print
//! the same way, apart from the width of the change code (11 vs 9 columns).
//!
//! Every run is bounded: ssh never prompts (`BatchMode`), gives up on a dead
//! connection, and the child is killed at [`Runner::timeout`]. A wedged
//! transfer would otherwise hold the head of home's queue for the whole fleet.

mod child;
mod client;
mod parse;
mod scratch;
mod spill;

pub use child::{Abort, Stopped};
pub use client::Client;
pub use parse::parse;

use crate::exclude::Excludes;
use crate::marker::Direction;
use crate::{Error, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// ssh's options for every transfer to home: never prompt (a host-key or
/// check-mode prompt would wait forever on a daemon), and notice a dead peer.
pub const SSH_OPTIONS: &str =
    "-o BatchMode=yes -o ConnectTimeout=10 -o ServerAliveInterval=15 -o ServerAliveCountMax=2";

/// How rsync is run: the same for every unit of a plan.
#[derive(Debug, Clone)]
pub struct Runner {
    /// Wall-clock bound on one rsync; the child is killed past it.
    pub timeout: Duration,
    /// The ssh program rsync runs for a transfer over ssh.
    pub ssh: String,
    /// The local rsync, which decides how home's path is quoted.
    pub client: Client,
    /// A private directory (the config dir's) for the empty stand-in and a
    /// long exclude list's file.
    pub scratch: PathBuf,
    /// The rsync program: `rsync` on `PATH`. Tests stand a script in.
    pub program: String,
    /// Raised to stop whatever this runner is running (a lost lease).
    pub abort: Abort,
}

impl Runner {
    pub fn new(timeout: Duration, client: Client, scratch: PathBuf) -> Self {
        Self {
            timeout,
            ssh: "ssh".into(),
            client,
            scratch,
            program: "rsync".into(),
            abort: Abort::default(),
        }
    }

    /// Run `args`, turning a stop into the error that explains it.
    fn run(&self, args: &[String]) -> Result<child::Finished> {
        let out = child::run(&self.program, args, self.timeout, &self.abort)?;
        match out.stopped {
            None => Ok(out),
            Some(why) => Err(stopped_error(&self.program, args, self.timeout, why)),
        }
    }
}

/// What a stopped rsync is reported as.
fn stopped_error(program: &str, args: &[String], timeout: Duration, why: Stopped) -> Error {
    match why {
        Stopped::TimedOut => Error::Rsync(format!(
            "{program} {} did not finish within {}s; killed it",
            args.join(" "),
            timeout.as_secs()
        )),
        Stopped::Aborted => Error::Rsync(format!(
            "{program} {} was stopped: this run's lease on home's queue was lost",
            args.join(" ")
        )),
    }
}

/// rsync's "some files vanished before they could be transferred". Benign in
/// a dry run of a live tree; reported, not fatal.
const VANISHED: i32 = 24;

/// One rsync of one unit.
#[derive(Debug, Clone)]
pub struct Transfer {
    pub direction: Direction,
    pub leaf_path: PathBuf,
    pub home_path: PathBuf,
    /// The ssh destination for home's side, or `None` for a same-machine home.
    pub home_host: Option<String>,
    /// A single-file unit: no trailing slashes.
    pub is_file: bool,
    pub excludes: Excludes,
    /// Use an empty directory as the source: the source unit does not exist
    /// (a deletion being pushed or pulled).
    pub source_absent: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Create,
    Update,
    /// Metadata only (permissions, times).
    Attr,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub kind: ChangeKind,
    /// Relative to the unit, without a trailing slash.
    pub path: String,
    pub is_dir: bool,
}

/// A parsed dry run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Itemized {
    pub changes: Vec<Change>,
    /// Lines rsync printed that were not itemized changes.
    pub notes: Vec<String>,
}

impl Itemized {
    /// Files (and links) created or updated.
    pub fn files_changed(&self) -> usize {
        self.count(|c| !c.is_dir && matches!(c.kind, ChangeKind::Create | ChangeKind::Update))
    }
    /// Files (and links) deleted.
    pub fn deleted(&self) -> usize {
        self.count(|c| !c.is_dir && c.kind == ChangeKind::Delete)
    }
    /// Whether anything at all would change.
    pub fn is_empty(&self) -> bool {
        self.changes
            .iter()
            .all(|c| c.kind == ChangeKind::Attr && c.is_dir)
    }
    fn count(&self, f: impl Fn(&Change) -> bool) -> usize {
        self.changes.iter().filter(|c| f(c)).count()
    }
    /// Paths whose bytes would be replaced or removed on the target.
    pub fn touched(&self) -> impl Iterator<Item = &str> {
        self.changes
            .iter()
            .filter(|c| !c.is_dir && c.kind != ChangeKind::Attr)
            .map(|c| c.path.as_str())
    }
}

impl Transfer {
    /// One end of the command line: `host:/path/`, `/path/`, or the empty
    /// stand-in for an absent source.
    fn endpoint(&self, runner: &Runner, home_side: bool, empty: &Path) -> String {
        let is_source = home_side == (self.direction == Direction::Pull);
        if is_source && self.source_absent {
            return format!("{}/", empty.display());
        }
        let p = if home_side {
            &self.home_path
        } else {
            &self.leaf_path
        };
        let mut s = p.display().to_string();
        if !self.is_file {
            s.push('/');
        }
        match (&self.home_host, home_side) {
            (Some(h), true) => format!("{h}:{}", runner.client.remote_path(&s)),
            _ => s,
        }
    }

    /// The full argument list. `empty` is the directory standing in for an
    /// absent source.
    pub fn args(&self, runner: &Runner, empty: &Path) -> Vec<String> {
        let mut a: Vec<String> = ["-a", "--delete", "--dry-run", "--itemize-changes"]
            .map(String::from)
            .into();
        a.push("--out-format=%i %n".into());
        // Names as raw bytes: rsync 3 under a daemon with no LANG escapes
        // high-bit bytes as \#ooo, which guardrail 1 could not match against
        // git's raw UTF-8 paths.
        a.push("-8".into());
        if self.home_host.is_some() {
            a.push("-e".into());
            a.push(format!("{} {SSH_OPTIONS}", runner.ssh));
        }
        a.extend(self.excludes.rsync_args());
        let home = self.endpoint(runner, true, empty);
        let leaf = self.endpoint(runner, false, empty);
        match self.direction {
            Direction::Pull => a.extend([home, leaf]),
            Direction::Push => a.extend([leaf, home]),
        }
        a
    }

    /// Run the dry run and parse what it would do.
    pub fn dry_run(&self, runner: &Runner) -> Result<Itemized> {
        if self.is_file && self.source_absent {
            // rsync cannot express "delete this one file"; the answer is known.
            return Ok(Itemized {
                changes: vec![Change {
                    kind: ChangeKind::Delete,
                    path: String::new(),
                    is_dir: false,
                }],
                notes: vec![],
            });
        }
        let empty = match self.source_absent {
            true => Some(scratch::EmptyDir::new(&runner.scratch)?),
            false => None,
        };
        let args = self.args(runner, empty.as_ref().map_or(Path::new(""), |e| e.path()));
        // A long ignored list rides in a file; it must outlive the child.
        let (args, excludes_file) = spill::spill(args, &runner.scratch, spill::LIMIT)?;
        debug_assert!(args.iter().any(|a| a == "--dry-run"));
        let out = runner.run(&args)?;
        drop(empty);
        drop(excludes_file);
        let code = out.code.unwrap_or(-1);
        if code != 0 && code != VANISHED {
            return Err(Error::Rsync(format!(
                "rsync {} exited {code}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        let mut parsed = parse(&String::from_utf8_lossy(&out.stdout));
        if code == VANISHED {
            parsed
                .notes
                .push("rsync: some files vanished during the dry run".into());
        }
        Ok(parsed)
    }
}

#[cfg(test)]
#[path = "rsync_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "rsync/ssh_tests.rs"]
mod ssh_tests;

#[cfg(test)]
#[path = "rsync/client_tests.rs"]
mod client_tests;
