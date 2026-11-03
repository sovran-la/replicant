//! The data plane: build an rsync command and parse its itemized output.
//!
//! replicant never copies a byte itself. A plan runs [`Transfer::dry_run`],
//! whose arguments always carry `--dry-run`; only `sync`'s apply runs
//! [`Transfer::apply`], the same arguments without it (`rsync/apply.rs`).
//!
//! Output is read with `--itemize-changes --out-format=%i %n`, which both
//! rsync 3 (Linux) and openrsync (macOS's /usr/bin/rsync, protocol 29) print
//! the same way, apart from the width of the change code (11 vs 9 columns).
//!
//! Every run is bounded: ssh never prompts (`BatchMode`), gives up on a dead
//! connection, and the child is killed at [`Runner::timeout`]. A wedged
//! transfer would otherwise hold the head of home's queue for the whole fleet.

mod apply;
mod child;
mod client;
mod parse;
mod scratch;
mod spill;

pub use crate::abort::Abort;
pub use crate::changes::{Change, ChangeKind, Itemized};
pub use apply::{Ran, copy_listed, delete_listed};
pub use child::Stopped;
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
            Some(why) => Err(stopped_error(
                &self.program,
                args,
                self.timeout,
                why,
                self.abort.why(),
            )),
        }
    }
}

/// What a stopped rsync is reported as.
fn stopped_error(
    program: &str,
    args: &[String],
    timeout: Duration,
    why: Stopped,
    aborted_because: Option<String>,
) -> Error {
    match why {
        Stopped::TimedOut => Error::Rsync(format!(
            "{program} {} did not finish within {}s; killed it",
            args.join(" "),
            timeout.as_secs()
        )),
        Stopped::Aborted => Error::Rsync(format!(
            "{program} {} was stopped: {}",
            args.join(" "),
            aborted_because.unwrap_or_else(|| "this run's lease on home's queue was lost".into())
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

    /// The dry run's full argument list. `empty` is the directory standing
    /// in for an absent source.
    pub fn args(&self, runner: &Runner, empty: &Path) -> Vec<String> {
        self.build(runner, empty, true)
    }

    /// The argument list, with `--dry-run` or (apply only) without.
    fn build(&self, runner: &Runner, empty: &Path, dry: bool) -> Vec<String> {
        let mut a: Vec<String> = ["-a", "--delete"].map(String::from).into();
        if dry {
            a.push("--dry-run".into());
        }
        a.push("--itemize-changes".into());
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
