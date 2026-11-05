//! The git invocations replicant makes, all of them read-only.
//!
//! **Read-only is a property of the flags, not of the commands.** `git status`
//! refreshes the index's stat cache opportunistically, which writes
//! `.git/index` — a synced path, and the very file whose per-machine stat data
//! caused PLAN.md's failure 1. Every call here runs with `--no-optional-locks`
//! and `GIT_OPTIONAL_LOCKS=0`, which turns that refresh off, and with
//! `core.fsmonitor=false`: a repo with fsmonitor enabled would otherwise have
//! `git status` start a daemon (or run a hook) that writes into `.git/`.
//!
//! **Only the repo's own `.gitignore` files decide what is ignored** (PLAN.md
//! rev 7). Every call runs with `core.excludesFile=/dev/null`: the user's
//! global gitignore differs per machine, and a file it hides is still synced
//! (`repos::ignored` does not read it), so the hash must see it as untracked
//! work. `.git/info/exclude` cannot be switched off for `git status`; a file
//! ignored only there still syncs but is invisible to the hash (pinned in
//! `state_ignored_tests.rs`).

use crate::{Error, Result};
use std::path::Path;
use std::process::Command;

/// One `git status --porcelain=v2` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    /// The entry as git printed it (both paths, for a rename). Hashed as is:
    /// it carries XY, the modes and the HEAD/index blob ids.
    pub raw: String,
    /// The path, relative to the repo.
    pub path: String,
    /// A rename's or copy's source.
    pub orig: Option<String>,
    /// The working tree differs from the index (or the path is untracked), so
    /// the working-tree bytes are state git cannot name by blob id.
    pub worktree_changed: bool,
    /// The worktree side of the change is a deletion.
    pub worktree_deleted: bool,
}

/// What git says about one repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoStatus {
    /// `.git/HEAD`, trimmed: `ref: refs/heads/main` or a detached object id.
    pub head: String,
    /// `for-each-ref` output: every branch, tag, remote ref and `refs/stash`.
    pub refs: String,
    pub entries: Vec<StatusEntry>,
}

fn run(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    run_with(repo, args, None)
}

/// [`run`], with the index file git should use when it is not the repo's
/// own: a copy git may refresh and rewrite without touching the repo.
fn run_with(repo: &Path, args: &[&str], index: Option<&Path>) -> Result<Vec<u8>> {
    let mut cmd = Command::new("git");
    cmd.env_remove("GIT_INDEX_FILE");
    if let Some(i) = index {
        cmd.env("GIT_INDEX_FILE", i);
    }
    let out = cmd
        .arg("--no-optional-locks")
        .args(["-c", "core.fsmonitor=false"])
        .args(["-c", "core.excludesFile=/dev/null"])
        .args(args)
        .current_dir(repo)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .map_err(|e| Error::Git(format!("could not run git in {}: {e}", repo.display())))?;
    if !out.status.success() {
        return Err(Error::Git(format!(
            "git {} in {} failed: {}",
            args.join(" "),
            repo.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

/// HEAD, refs and status of the repo at `repo`.
pub fn status(repo: &Path) -> Result<RepoStatus> {
    let head = std::fs::read_to_string(repo.join(".git/HEAD"))
        .map_err(|e| Error::Git(format!("{}: unreadable .git/HEAD: {e}", repo.display())))?
        .trim()
        .to_string();
    let refs = run(repo, &["for-each-ref", "--format=%(refname) %(objectname)"])?;
    let porcelain = run(
        repo,
        &["status", "--porcelain=v2", "-z", "--untracked-files=all"],
    )?;
    let in_repo = |e| match e {
        Error::Git(m) => Error::Git(format!("{}: {m}", repo.display())),
        other => other,
    };
    Ok(RepoStatus {
        head,
        refs: decode_refs(&refs).map_err(in_repo)?,
        entries: parse_porcelain_v2(&porcelain).map_err(in_repo)?,
    })
}

/// Decode `for-each-ref` output. A ref name that is not UTF-8 is refused by
/// name, as [`parse_porcelain_v2`] refuses a path: decoding it lossily would
/// hash a ref that does not exist, and two different names could hash alike.
pub fn decode_refs(out: &[u8]) -> Result<String> {
    String::from_utf8(out.to_vec()).map_err(|_| {
        let bad = out
            .split(|b| *b == b'\n')
            .find(|l| std::str::from_utf8(l).is_err())
            .unwrap_or(out);
        Error::Git(format!(
            "git for-each-ref reports a ref name that is not UTF-8, which replicant \
             cannot hash: {:?}; rename it",
            String::from_utf8_lossy(bad)
        ))
    })
}

/// Parse `git status --porcelain=v2 -z` output.
///
/// A path that is not UTF-8 (possible on Linux) is refused by name: decoding
/// it lossily would hash a path that does not exist and hide the file from
/// guardrail 1.
pub fn parse_porcelain_v2(out: &[u8]) -> Result<Vec<StatusEntry>> {
    let tokens = out
        .split(|b| *b == 0)
        .filter(|t| !t.is_empty())
        .map(|t| {
            std::str::from_utf8(t).map_err(|_| {
                Error::Git(format!(
                    "git status reports a path that is not UTF-8, which replicant cannot \
                     hash or guard: {:?}; rename it",
                    String::from_utf8_lossy(t)
                ))
            })
        })
        .collect::<Result<Vec<&str>>>()?;
    let mut tokens = tokens.into_iter();
    let mut entries = Vec::new();
    let bad = |t: &str| Error::Git(format!("unreadable porcelain v2 entry {t:?}"));
    while let Some(t) = tokens.next() {
        let (kind, rest) = t.split_at(1);
        match kind {
            "1" | "2" | "u" => {
                // Fields between the kind and the path:
                // `1`: XY sub mH mI mW hH hI            → 7
                // `2`: XY sub mH mI mW hH hI Xscore     → 8
                // `u`: XY sub m1 m2 m3 mW h1 h2 h3      → 9
                let n = match kind {
                    "1" => 7,
                    "2" => 8,
                    _ => 9,
                };
                let fields: Vec<&str> = rest.trim_start().splitn(n + 1, ' ').collect();
                let (Some(xy), Some(path)) = (fields.first(), fields.get(n)) else {
                    return Err(bad(t));
                };
                let y = xy.chars().nth(1).ok_or_else(|| bad(t))?;
                let orig = if kind == "2" {
                    Some(tokens.next().ok_or_else(|| bad(t))?.to_string())
                } else {
                    None
                };
                let raw = match &orig {
                    Some(o) => format!("{t}\0{o}"),
                    None => t.to_string(),
                };
                entries.push(StatusEntry {
                    raw,
                    path: path.to_string(),
                    orig,
                    worktree_changed: kind == "u" || y != '.',
                    worktree_deleted: y == 'D',
                });
            }
            "?" => entries.push(StatusEntry {
                raw: t.to_string(),
                path: rest.trim_start().to_string(),
                orig: None,
                worktree_changed: true,
                worktree_deleted: false,
            }),
            "!" | "#" => {}
            _ => return Err(bad(t)),
        }
    }
    Ok(entries)
}

/// The tracked paths whose working-tree content differs from HEAD, both
/// names of a rename: what a sync that replaced the working tree would lose
/// (guardrail 1). The index is not consulted: a staged change whose file
/// on disk still matches HEAD is index bookkeeping, not work, and an index
/// carried over from another machine says "modified" about files that are
/// not (PLAN.md, "The problem", 1). An unborn HEAD has nothing to differ
/// from: every tracked path counts.
///
/// `git diff` refreshes the index's stat cache and writes it back, which
/// `--no-optional-locks` does not stop; a status must write nothing in the
/// repo, so the diff runs against a throwaway copy of the index.
pub fn worktree_vs_head(repo: &Path) -> Result<Vec<String>> {
    // `--git-path` answers for a linked worktree too, whose `.git` is a file.
    let own = run(repo, &["rev-parse", "--git-path", "index"])?;
    let own = repo.join(String::from_utf8_lossy(&own).trim());
    if !own.exists() {
        return tracked(repo);
    }
    let scratch = Scratch::copy_of(&own)?;
    let args = ["diff", "--no-renames", "--name-only", "-z", "HEAD", "--"];
    let out = match run_with(repo, &args, Some(&scratch.0)) {
        Ok(out) => out,
        Err(_) if run(repo, &["rev-parse", "--verify", "-q", "HEAD"]).is_err() => {
            return tracked(repo);
        }
        Err(e) => return Err(e),
    };
    Ok(out
        .split(|b| *b == 0)
        .filter(|t| !t.is_empty())
        .filter_map(|t| std::str::from_utf8(t).ok().map(str::to_string))
        .collect())
}

/// A copy of a file in the temp directory, gone when dropped.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn copy_of(src: &Path) -> Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            "replicant-index-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(name);
        // Ours alone: created new, never reused.
        std::fs::File::options()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let s = Self(path);
        std::fs::copy(src, &s.0)?;
        Ok(s)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Every path the repo's index tracks, sorted as git sorts them. A tracked
/// path is never ignored, whatever a `.gitignore` says (`repos::ignored`).
/// A name that is not UTF-8 is left out: no path in this crate can hold it,
/// and the ignored list, which skips such names too, can never name it.
pub fn tracked(repo: &Path) -> Result<Vec<String>> {
    let out = run(repo, &["ls-files", "-z", "--cached"])?;
    Ok(out
        .split(|b| *b == 0)
        .filter(|t| !t.is_empty())
        .filter_map(|t| std::str::from_utf8(t).ok().map(str::to_string))
        .collect())
}

/// How many entries the repo's index holds: its tracked files. Read from the
/// index header (`DIRC`, version, count), so it costs no git process. A repo
/// with no index yet has none. Under `core.splitIndex` this counts only the
/// split part; nothing here enables that.
pub fn index_entries(repo: &Path) -> Result<u64> {
    use std::io::Read;
    let path = repo.join(".git/index");
    let mut f = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.into()),
    };
    let mut head = [0u8; 12];
    f.read_exact(&mut head)
        .ok()
        .filter(|_| &head[..4] == b"DIRC")
        .ok_or_else(|| Error::Git(format!("{}: not a git index", path.display())))?;
    Ok(u32::from_be_bytes([head[8], head[9], head[10], head[11]]).into())
}

/// Lock files inside a repo's `.git` (not its objects, not its worktrees'),
/// relative to the repo: `.git/index.lock`, `.git/refs/heads/main.lock`.
///
/// Guardrail 3 reads "any `*.lock` on the source" as any `*.lock` git itself
/// holds. A `Cargo.lock` in the working tree is a source file, not a write in
/// progress.
pub fn locks(repo: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let git = repo.join(".git");
    if git.is_dir() {
        find_locks(&git, ".git", &mut out)?;
    }
    out.sort();
    Ok(out)
}

fn find_locks(abs: &Path, rel: &str, out: &mut Vec<String>) -> Result<()> {
    for item in std::fs::read_dir(abs)? {
        let item = item?;
        let Some(name) = item.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let child = format!("{rel}/{name}");
        let ft = item.file_type()?;
        if ft.is_dir() {
            if child != ".git/objects" && child != ".git/worktrees" {
                find_locks(&item.path(), &child, out)?;
            }
        } else if name.ends_with(".lock") {
            out.push(child);
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "git_tests.rs"]
mod tests;
