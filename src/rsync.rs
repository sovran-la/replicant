//! The data plane: build an rsync command and parse its itemized output.
//!
//! replicant never copies a byte itself. In v0.1 it never runs rsync for real
//! either: [`Transfer::args`] always carries `--dry-run`, and there is no code
//! path that omits it.
//!
//! Output is read with `--itemize-changes --out-format=%i %n`, which both
//! rsync 3 (Linux) and openrsync (macOS's /usr/bin/rsync, protocol 29) print
//! the same way, apart from the width of the change code (11 vs 9 columns).

use crate::exclude::Excludes;
use crate::marker::Direction;
use crate::{Error, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

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
    fn endpoint(&self, home_side: bool, empty: &Path) -> String {
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
            (Some(h), true) => format!("{h}:{s}"),
            _ => s,
        }
    }

    /// The full argument list. `empty` is the directory standing in for an
    /// absent source.
    pub fn args(&self, empty: &Path) -> Vec<String> {
        let mut a: Vec<String> = ["-a", "--delete", "--dry-run", "--itemize-changes"]
            .map(String::from)
            .into();
        a.push("--out-format=%i %n".into());
        a.extend(self.excludes.rsync_args());
        let home = self.endpoint(true, empty);
        let leaf = self.endpoint(false, empty);
        match self.direction {
            Direction::Pull => a.extend([home, leaf]),
            Direction::Push => a.extend([leaf, home]),
        }
        a
    }

    /// Run the dry run and parse what it would do.
    pub fn dry_run(&self) -> Result<Itemized> {
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
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let empty =
            std::env::temp_dir().join(format!("replicant-empty-{}-{n}", std::process::id()));
        if self.source_absent {
            std::fs::create_dir_all(&empty)?;
        }
        let args = self.args(&empty);
        debug_assert!(args.iter().any(|a| a == "--dry-run"));
        let out = Command::new("rsync")
            .args(&args)
            .output()
            .map_err(|e| Error::Rsync(format!("could not run rsync: {e}")))?;
        if self.source_absent {
            let _ = std::fs::remove_dir(&empty);
        }
        let code = out.status.code().unwrap_or(-1);
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

/// Parse `--itemize-changes --out-format=%i %n` output.
pub fn parse(text: &str) -> Itemized {
    let mut out = Itemized::default();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        if let Some(rest) = line.strip_prefix("*deleting") {
            let name = rest.trim_start();
            out.changes.push(Change {
                kind: ChangeKind::Delete,
                is_dir: name.ends_with('/'),
                path: name.trim_end_matches('/').to_string(),
            });
            continue;
        }
        let Some((code, name)) = line.split_once(' ') else {
            out.notes.push(line.to_string());
            continue;
        };
        let b = code.as_bytes();
        let shaped = b.len() >= 9 && b"<>ch.".contains(&b[0]) && b"fdLDS".contains(&b[1]);
        if !shaped {
            out.notes.push(line.to_string());
            continue;
        }
        let kind = if b[2..].iter().all(|c| *c == b'+') {
            ChangeKind::Create
        } else if b[0] == b'.' {
            ChangeKind::Attr
        } else {
            ChangeKind::Update
        };
        let name = name.strip_suffix('/').unwrap_or(name);
        if (name == "." || name.is_empty()) && kind == ChangeKind::Attr {
            continue;
        }
        out.changes.push(Change {
            kind,
            is_dir: b[1] == b'd',
            path: name.to_string(),
        });
    }
    // openrsync prints a deleted directory without its trailing slash, so a
    // deletion is a directory when other deletions lie beneath it.
    let deleted: Vec<String> = out
        .changes
        .iter()
        .filter(|c| c.kind == ChangeKind::Delete)
        .map(|c| format!("{}/", c.path))
        .collect();
    for c in out
        .changes
        .iter_mut()
        .filter(|c| c.kind == ChangeKind::Delete && !c.is_dir)
    {
        let prefix = format!("{}/", c.path);
        c.is_dir = deleted
            .iter()
            .any(|d| d.len() > prefix.len() && d.starts_with(&prefix));
    }
    out
}

#[cfg(test)]
#[path = "rsync_tests.rs"]
mod tests;
