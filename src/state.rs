//! The state hash. PLAN.md: "a hash over per-repo `(HEAD, dirty-tree hash)`
//! plus a manifest of non-repo files".
//!
//! - **Repo**: HEAD, every ref except remote-tracking ones (branches, tags,
//!   `refs/stash` — the plan's requirements list branches and stashes as
//!   state, so `HEAD` here means all of them), and the dirty tree: each
//!   porcelain v2 entry, which names staged
//!   content by blob id, plus the working-tree *metadata* of anything the
//!   index does not describe (modified, untracked, deleted): kind, size,
//!   mtime to the nanosecond, mode, link target.
//! - **Loose**: the sorted manifest of (path, kind, size, mtime, mode, link
//!   target).
//!
//! Neither reads file contents. A transfer keeps mtimes, and a baseline is
//! compared with the same machine's next hash, so metadata is enough. Content
//! was tried first for dirty repo files and cost 1.7 GB of reads per `status`
//! on thistledown (2026-11-01): a modified 400 MB zip is still a modified
//! 400 MB zip. The one thing metadata cannot see is a same-size rewrite that
//! lands in the same nanosecond, which is the same blind spot the loose
//! manifest has always had.
//!
//! **Decision: `refs/remotes/*` is not state** (v0.1 review, Q3). An IDE that
//! autofetches moves remote-tracking refs on both machines independently, which
//! made every such repo a "both changed" conflict for no reason. They are a
//! cache of another server, refetchable, and a transfer still carries them with
//! `.git`. Everything else `for-each-ref` lists is hashed, including anything
//! outside `refs/heads`, `refs/tags` and `refs/stash` (notes, replace refs).
//!
//! **Git-ignored files are not state** (PLAN.md rev 7): they do not sync.
//! `git status` without `--ignored` never reports them; the unit's excludes
//! carry them anyway ([`crate::repos::ignored`]), so nothing here could.
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

    /// The first 12 hex digits, for display. The value may come from the
    /// wire, so a non-ASCII one is shown whole rather than cut mid-character.
    pub fn short(&self) -> &str {
        match self {
            UnitState::Empty => "empty",
            UnitState::Present(h) => h.get(..12).unwrap_or(h),
        }
    }
}

/// A repo's state, and the facts the guardrails need from the same look.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoFacts {
    pub state: UnitState,
    /// `.git/HEAD`, for display.
    pub head: String,
    /// Uncommitted work, relative to the repo (guardrail 1, on the target):
    /// tracked paths whose working tree differs from HEAD, and untracked
    /// paths. Not a staged change whose file on disk matches HEAD: that is
    /// index state, which the hash counts and a sync replaces, and which an
    /// index from another machine reports falsely.
    pub dirty: Vec<String>,
    /// Git lock files, relative to the repo (guardrail 3, on the source).
    pub locks: Vec<String>,
    /// Tracked files plus untracked ones: what removing the working tree
    /// would remove, for "deleted on X" (`.git` itself not counted).
    pub files: u64,
}

/// Remote-tracking refs, left out of the repo hash (module doc).
const REMOTE_REFS: &str = "refs/remotes/";

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
    for line in st.refs.lines().filter(|l| !l.starts_with(REMOTE_REFS)) {
        h.update(line.as_bytes());
        h.update(b"\n");
    }
    h.update(b"\0dirty\0");
    let mut dirty = Vec::new();
    let mut ambiguous = Vec::new();
    let mut untracked = 0;
    for e in &st.entries {
        if ex.path_excluded(&e.path, e.path.ends_with('/')) {
            continue;
        }
        h.update(e.raw.as_bytes());
        h.update(b"\0");
        untracked += u64::from(e.raw.starts_with('?'));
        if e.worktree_changed {
            content(&mut h, &abs.join(&e.path), e.worktree_deleted)?;
        }
        match &e.tracked {
            // Untracked, or unmerged: work.
            None => dirty.push(e.path.clone()),
            // Staged, then changed on disk: HEAD's content again, or not.
            Some(t) if t.needs_hash() && e.orig.is_none() => ambiguous.push((e, t)),
            // The columns decide; a rename counts both names.
            Some(_) => {
                dirty.push(e.path.clone());
                if let Some(o) = &e.orig {
                    dirty.push(o.clone());
                }
            }
        }
    }
    // Only these files are read: a phantom from a copied index is usually
    // a handful, and the rest of the repo costs nothing (rev 18).
    if !ambiguous.is_empty() {
        let paths: Vec<&str> = ambiguous.iter().map(|(e, _)| e.path.as_str()).collect();
        let ids = git::blob_ids(abs, &paths)?;
        for ((e, t), id) in ambiguous.iter().zip(ids) {
            if id != t.blob_head || t.mode_worktree != t.mode_head {
                dirty.push(e.path.clone());
            }
        }
    }
    dirty.sort();
    dirty.dedup();
    Ok(RepoFacts {
        state: UnitState::Present(hex(h)),
        head: st.head,
        dirty,
        locks: git::locks(abs)?,
        files: git::index_entries(abs)? + untracked,
    })
}

/// Feed a working-tree path's metadata (or link target, or absence) to `h`.
/// Never its bytes; see the module doc.
fn content(h: &mut Sha256, path: &Path, deleted: bool) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let meta = match std::fs::symlink_metadata(path) {
        // A socket or FIFO in a tracked file's place: not state, as anywhere.
        Ok(m) if !deleted && !crate::repos::is_special(&m) => m,
        _ => {
            h.update(b"<absent>\0");
            return Ok(());
        }
    };
    if meta.file_type().is_symlink() {
        h.update(b"<link>");
        h.update(std::fs::read_link(path)?.to_string_lossy().as_bytes());
    } else if meta.is_file() {
        h.update(
            format!(
                "<file>{}\0{}.{:09}\0{:o}",
                meta.len(),
                meta.mtime(),
                meta.mtime_nsec(),
                meta.permissions().mode()
            )
            .as_bytes(),
        );
    } else {
        h.update(b"<dir>");
    }
    h.update(b"\0");
    Ok(())
}

/// How many non-directory entries a loose manifest holds.
pub fn loose_files(entries: &[Entry]) -> u64 {
    entries.iter().filter(|e| e.kind != Kind::Dir).count() as u64
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

#[cfg(test)]
#[path = "state_ignored_tests.rs"]
mod ignored_tests;
