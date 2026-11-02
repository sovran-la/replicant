//! Real runs: a unit's transfer without `--dry-run`, and the two list
//! operations home's push backups and rollbacks are made of.
//!
//! A real run's outcome is a [`Ran`], also when rsync failed or was stopped:
//! what it printed before it died is the record of what it may have changed,
//! which the journal, the push manifest and a rollback need.

use super::{Itemized, Runner, Stopped, Transfer, VANISHED, child, parse, scratch, spill};
use crate::exclude::Excludes;
use crate::marker::Direction;
use crate::{Error, Result};
use std::path::Path;

/// What a real rsync did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    /// What rsync reported changing, also when it then failed.
    pub itemized: Itemized,
    /// `None`: killed by a signal.
    pub code: Option<i32>,
    pub stopped: Option<Stopped>,
    pub stderr: String,
}

impl Ran {
    /// Exit 0 and not stopped: the only outcome after which a baseline may
    /// be written. Exit 24 (files vanished) is not success here: the set
    /// that arrived is not the set the source holds.
    pub fn ok(&self) -> bool {
        self.stopped.is_none() && self.code == Some(0)
    }

    /// Why it did not succeed, for the journal: `rsync exit 23`.
    pub fn why(&self) -> String {
        match (self.stopped, self.code) {
            (Some(Stopped::Aborted), _) => "lease lost; rsync stopped".into(),
            (Some(Stopped::TimedOut), _) => "rsync timed out; killed".into(),
            (None, Some(VANISHED)) => format!("rsync exit {VANISHED}: files vanished"),
            (None, Some(c)) => format!("rsync exit {c}"),
            (None, None) => "rsync killed by a signal".into(),
        }
    }

    fn from(out: child::Finished) -> Self {
        let mut itemized = parse(&String::from_utf8_lossy(&out.stdout));
        // openrsync can print one deletion twice; a change is a change once.
        let mut seen = std::collections::HashSet::new();
        itemized
            .changes
            .retain(|c| seen.insert((c.kind, c.path.clone())));
        Ran {
            itemized,
            code: out.code,
            stopped: out.stopped,
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        }
    }
}

/// `dir/` as an rsync endpoint, on `host` if there is one.
fn dir_endpoint(runner: &Runner, host: Option<&str>, dir: &Path) -> String {
    let s = format!("{}/", dir.display());
    match host {
        Some(h) => format!("{h}:{}", runner.client.remote_path(&s)),
        None => s,
    }
}

fn ssh_args(runner: &Runner, host: Option<&str>) -> Vec<String> {
    match host {
        Some(_) => vec![
            "-e".into(),
            format!("{} {}", runner.ssh, super::SSH_OPTIONS),
        ],
        None => vec![],
    }
}

impl Transfer {
    /// Carry the transfer out: the dry run's arguments without `--dry-run`.
    pub fn apply(&self, runner: &Runner) -> Result<Ran> {
        if self.is_file && self.source_absent {
            return self.delete_file(runner);
        }
        let empty = match self.source_absent {
            true => Some(scratch::EmptyDir::new(&runner.scratch)?),
            false => None,
        };
        let args = self.build(
            runner,
            empty.as_ref().map_or(Path::new(""), |e| e.path()),
            false,
        );
        let (args, excludes_file) = spill::spill(args, &runner.scratch, spill::LIMIT)?;
        let out = child::run(&runner.program, &args, runner.timeout, &runner.abort)?;
        drop(empty);
        drop(excludes_file);
        Ok(Ran::from(out))
    }

    /// A single-file unit's deletion: its directory, with only its name let
    /// through the filter, from an empty source.
    fn delete_file(&self, runner: &Runner) -> Result<Ran> {
        let target = match self.direction {
            Direction::Pull => &self.leaf_path,
            Direction::Push => &self.home_path,
        };
        let (Some(dir), Some(name)) = (target.parent(), target.file_name()) else {
            return Err(Error::Rsync(format!(
                "{} has no parent directory to delete it from",
                target.display()
            )));
        };
        let name = name
            .to_str()
            .ok_or_else(|| Error::Rsync(format!("{} is not UTF-8", target.display())))?;
        let host = match self.direction {
            Direction::Push => self.home_host.as_deref(),
            Direction::Pull => None,
        };
        let mut ran = delete_listed(runner, host, dir, &[(name.to_string(), false)])?;
        // The unit is the file: the dry run calls it "" (the unit itself).
        for c in &mut ran.itemized.changes {
            if c.path == name {
                c.path.clear();
            }
        }
        Ok(ran)
    }
}

/// The rules that let exactly `paths` (relative to the transfer root,
/// `(path, is_dir)`) through, with their ancestors, and nothing else.
fn include_rules(paths: &[(String, bool)]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (p, is_dir) in paths {
        let comps: Vec<&str> = p.split('/').collect();
        for i in 1..comps.len() {
            let r = Excludes::literal_rule(&comps[..i].join("/"), true);
            if !out.contains(&r) {
                out.push(r);
            }
        }
        let r = Excludes::literal_rule(p, *is_dir);
        if !out.contains(&r) {
            out.push(r);
        }
    }
    out
}

/// Delete exactly `paths` under `dir` (on `host`, if given) and nothing
/// else: an empty skeleton holding the listed paths' ancestors is the
/// source, and the filter lets only the listed paths and their ancestors
/// through. A listed directory goes with what is listed inside it;
/// anything unlisted inside it is excluded, so protected, and the directory
/// stays ("not empty, cannot delete").
///
/// The skeleton holds only ancestors that are real directories in `dir`
/// (not followed through a symlink): openrsync creates a source directory
/// the target lacks even under `--existing` (measured), and a skeleton
/// directory facing a symlink would replace the link. So paths below an
/// ancestor deeper than `dir` itself need `dir` to be local.
pub fn delete_listed(
    runner: &Runner,
    host: Option<&str>,
    dir: &Path,
    paths: &[(String, bool)],
) -> Result<Ran> {
    if host.is_some() && paths.iter().any(|(p, _)| p.contains('/')) {
        return Err(Error::Rsync(format!(
            "deleting nested paths under {} needs it to be local",
            dir.display()
        )));
    }
    let skel = scratch::EmptyDir::new(&runner.scratch)?;
    let listed_dirs: Vec<&str> = paths
        .iter()
        .filter(|(_, d)| *d)
        .map(|(p, _)| p.as_str())
        .collect();
    let mut made = Vec::new();
    for (p, _) in paths {
        let comps: Vec<&str> = p.split('/').collect();
        for i in 1..comps.len() {
            let a = comps[..i].join("/");
            let real = std::fs::symlink_metadata(dir.join(&a)).is_ok_and(|m| m.is_dir());
            if !real || listed_dirs.iter().any(|d| crate::paths::is_within(&a, d)) {
                break;
            }
            made.push(skel.path().join(&a));
        }
    }
    for d in &made {
        std::fs::create_dir_all(d)?;
    }
    let rules = include_rules(paths);
    let list = scratch::ListFile::new(
        &runner.scratch,
        "include-",
        rules.iter().map(String::as_str),
    )?;
    let mut args: Vec<String> = ["-r", "-l", "--delete", "--existing", "--itemize-changes"]
        .map(String::from)
        .into();
    args.push("--out-format=%i %n".into());
    args.push("-8".into());
    args.extend(ssh_args(runner, host));
    args.push(format!("--include-from={}", list.path().display()));
    args.push("--exclude=*".into());
    args.push(format!("{}/", skel.path().display()));
    args.push(dir_endpoint(runner, host, dir));
    let out = child::run(&runner.program, &args, runner.timeout, &runner.abort)?;
    // The skeleton's directories go before the skeleton itself.
    for d in made.iter().rev() {
        let _ = std::fs::remove_dir(d);
    }
    drop(list);
    drop(skel);
    Ok(Ran::from(out))
}

/// Copy the listed files (relative to `from`) to the same paths under `to`,
/// with their attributes. Local only: home backing up what a push is about
/// to change, and restoring it.
pub fn copy_listed(runner: &Runner, from: &Path, to: &Path, files: &[String]) -> Result<Ran> {
    let list = scratch::ListFile::new(&runner.scratch, "files-", files.iter().map(String::as_str))?;
    let mut args: Vec<String> = ["-a", "--itemize-changes"].map(String::from).into();
    args.push("--out-format=%i %n".into());
    args.push("-8".into());
    args.push(format!("--files-from={}", list.path().display()));
    args.push(format!("{}/", from.display()));
    args.push(format!("{}/", to.display()));
    let out = child::run(&runner.program, &args, runner.timeout, &runner.abort)?;
    drop(list);
    Ok(Ran::from(out))
}

#[cfg(test)]
#[path = "apply_tests.rs"]
mod tests;
