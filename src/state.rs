//! The state hash. PLAN.md: "a hash over per-repo `(HEAD, dirty-tree hash)`
//! plus a manifest of non-repo files".
//!
//! - **Repo**: HEAD, every ref (branches, tags, `refs/stash` — the plan's
//!   requirements list branches and stashes as state, so `HEAD` here means all
//!   of them), and the dirty tree: each porcelain v2 entry, which names staged
//!   content by blob id, plus the working-tree bytes of anything the index
//!   does not describe (modified, untracked, deleted).
//! - **Loose**: the sorted manifest of (path, kind, size, mtime, mode, link
//!   target). Not content: rsync `-a` preserves mtimes, and a baseline is
//!   compared with the same machine's next hash, so metadata is enough and
//!   hashing a dataset directory would not be.
//!
//! Status entries and manifest entries under an excluded path are dropped, so
//! a tracked file under an excluded `bin/` does not read as "deleted" on the
//! side it never reached.

use crate::Result;
use crate::exclude::Excludes;
use crate::git;
use crate::repos::{Entry, Kind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

/// One unit's state on one side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitState {
    /// Nothing there: the path does not exist, or holds nothing after excludes.
    Empty,
    /// Something there, with this hash.
    Present(String),
}

impl UnitState {
    pub fn is_empty(&self) -> bool {
        matches!(self, UnitState::Empty)
    }

    /// The first 12 hex digits, for display.
    pub fn short(&self) -> &str {
        match self {
            UnitState::Empty => "empty",
            UnitState::Present(h) => &h[..h.len().min(12)],
        }
    }
}

/// A repo's state, and the facts the guardrails need from the same look.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoFacts {
    pub state: UnitState,
    /// `.git/HEAD`, for display.
    pub head: String,
    /// Dirty paths, relative to the repo (guardrail 1, on the target).
    pub dirty: Vec<String>,
    /// Git lock files, relative to the repo (guardrail 3, on the source).
    pub locks: Vec<String>,
}

fn hex(d: Sha256) -> String {
    d.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Hash one repo. `ex` is relative to the repo directory.
pub fn repo(abs: &Path, ex: &Excludes) -> Result<RepoFacts> {
    let st = git::status(abs)?;
    let mut h = Sha256::new();
    h.update(b"repo\0head\0");
    h.update(st.head.as_bytes());
    h.update(b"\0refs\0");
    h.update(st.refs.as_bytes());
    h.update(b"\0dirty\0");
    let mut dirty = Vec::new();
    for e in &st.entries {
        if ex.path_excluded(&e.path, e.path.ends_with('/')) {
            continue;
        }
        h.update(e.raw.as_bytes());
        h.update(b"\0");
        if e.worktree_changed {
            content(&mut h, &abs.join(&e.path), e.worktree_deleted)?;
        }
        dirty.push(e.path.clone());
        if let Some(o) = &e.orig {
            dirty.push(o.clone());
        }
    }
    Ok(RepoFacts {
        state: UnitState::Present(hex(h)),
        head: st.head,
        dirty,
        locks: git::locks(abs)?,
    })
}

/// Feed a working-tree path's bytes (or link target, or absence) to `h`.
fn content(h: &mut Sha256, path: &Path, deleted: bool) -> Result<()> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) if !deleted => m,
        _ => {
            h.update(b"<absent>\0");
            return Ok(());
        }
    };
    if meta.file_type().is_symlink() {
        h.update(b"<link>");
        h.update(std::fs::read_link(path)?.to_string_lossy().as_bytes());
    } else if meta.is_file() {
        let mut f = std::fs::File::open(path)?;
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
    } else {
        h.update(b"<dir>");
    }
    h.update(b"\0");
    Ok(())
}

/// Hash a loose manifest. An empty one is [`UnitState::Empty`].
pub fn loose(entries: &[Entry]) -> UnitState {
    if entries.is_empty() {
        return UnitState::Empty;
    }
    let mut h = Sha256::new();
    h.update(b"loose\0");
    for e in entries {
        let kind = match e.kind {
            Kind::File => "f",
            Kind::Symlink => "l",
            Kind::Dir => "d",
        };
        h.update(
            format!(
                "{}\0{kind}\0{}\0{}\0{:o}\0{}\0",
                e.rel,
                e.size,
                e.mtime,
                e.mode,
                e.link.as_deref().unwrap_or("")
            )
            .as_bytes(),
        );
    }
    UnitState::Present(hex(h))
}

/// Run `f` over `items` on up to `workers` threads, results in input order.
///
/// The state hash is one `git status` per repo; across `~/work` that is the
/// cost PLAN.md's performance note worries about, and it parallelizes.
pub fn parallel_map<T: Sync, R: Send>(
    items: &[T],
    workers: usize,
    f: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<Option<R>>> = Mutex::new((0..items.len()).map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..workers.clamp(1, items.len().max(1)) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    let r = f(item);
                    out.lock().unwrap_or_else(|e| e.into_inner())[i] = Some(r);
                }
            });
        }
    });
    out.into_inner()
        .unwrap_or_else(|e| e.into_inner())
        .into_iter()
        .map(|r| r.expect("every index is filled exactly once"))
        .collect()
}

/// How many hashing threads to use.
pub fn workers() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(16)
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
