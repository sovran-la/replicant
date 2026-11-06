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
    /// What guardrail 1 needs to tell work from index bookkeeping, for a
    /// tracked entry: `XY`, HEAD's mode and blob, the worktree's mode. An
    /// untracked or unmerged entry has none.
    pub tracked: Option<Tracked>,
}

/// A tracked status entry's index-vs-HEAD and worktree-vs-index columns,
/// with the HEAD side git names by blob id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tracked {
    /// `X`: the index against HEAD. `.` means they agree.
    pub x: char,
    /// `Y`: the working tree against the index. `.` means they agree.
    pub y: char,
    pub mode_head: String,
    pub mode_worktree: String,
    pub blob_head: String,
}

impl Tracked {
    /// Whether the working tree may still match HEAD, which the columns
    /// alone cannot tell: staged as one thing (`X` set) and then changed on
    /// disk (`Y` is `M`), possibly back to HEAD's content. Everything else
    /// the columns decide: `Y` clear means the tree is the index, which `X`
    /// says differs from HEAD; `Y` set with `X` clear means it differs from
    /// the index, which is HEAD; a deletion, type change or added entry is
    /// never HEAD's.
    pub fn needs_hash(&self) -> bool {
        self.x != '.' && self.y == 'M'
    }
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

/// `git` in `repo`, read-only: no optional locks, no fsmonitor, no
/// user-level excludes, none of the caller's `GIT_*` redirections.
fn git(repo: &Path, args: &[&str]) -> Command {
    let mut c = Command::new("git");
    c.arg("--no-optional-locks")
        .args(["-c", "core.fsmonitor=false"])
        .args(["-c", "core.excludesFile=/dev/null"])
        .args(args)
        .current_dir(repo)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    c
}

fn could_not_run(repo: &Path, e: std::io::Error) -> Error {
    Error::Git(format!("could not run git in {}: {e}", repo.display()))
}

fn run(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = git(repo, args)
        .output()
        .map_err(|e| could_not_run(repo, e))?;
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
                let mut xy = xy.chars();
                let (x, y) = (
                    xy.next().ok_or_else(|| bad(t))?,
                    xy.next().ok_or_else(|| bad(t))?,
                );
                // `1`/`2`: mH mI mW hH; `u` has no single HEAD side.
                let tracked = (kind != "u").then(|| Tracked {
                    x,
                    y,
                    mode_head: fields[2].to_string(),
                    mode_worktree: fields[4].to_string(),
                    blob_head: fields[5].to_string(),
                });
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
                    tracked,
                });
            }
            "?" => entries.push(StatusEntry {
                raw: t.to_string(),
                path: rest.trim_start().to_string(),
                orig: None,
                worktree_changed: true,
                worktree_deleted: false,
                tracked: None,
            }),
            "!" | "#" => {}
            _ => return Err(bad(t)),
        }
    }
    Ok(entries)
}

/// The blob ids git would store for these working-tree paths, in order:
/// what the file holds, through the repo's clean filters, as `git add`
/// would hash it. Reads only those files; writes nothing. A symlink's blob
/// is its target. A path git cannot hash (gone meanwhile, unreadable) gets
/// an empty id, which matches no HEAD blob.
pub fn blob_ids(repo: &Path, paths: &[&str]) -> Result<Vec<String>> {
    use std::io::Write;
    let mut ids = vec![String::new(); paths.len()];
    let mut files = Vec::new();
    for (i, p) in paths.iter().enumerate() {
        let abs = repo.join(p);
        match std::fs::symlink_metadata(&abs) {
            Ok(m) if m.file_type().is_symlink() => {
                let target = std::fs::read_link(&abs)?;
                let out = hash_stdin(repo, target.as_os_str().as_encoded_bytes())?;
                ids[i] = out;
            }
            Ok(m) if m.is_file() && !p.contains('\n') => files.push(i),
            _ => {}
        }
    }
    if files.is_empty() {
        return Ok(ids);
    }
    let mut child = git(repo, &["hash-object", "--stdin-paths"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| could_not_run(repo, e))?;
    {
        let mut stdin = child.stdin.take().expect("piped");
        for &i in &files {
            writeln!(stdin, "{}", paths[i])?;
        }
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(Error::Git(format!(
            "git hash-object in {} failed: {}",
            repo.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    for (&i, id) in files.iter().zip(text.lines()) {
        ids[i] = id.trim().to_string();
    }
    Ok(ids)
}

/// `git hash-object --stdin` of `bytes`.
fn hash_stdin(repo: &Path, bytes: &[u8]) -> Result<String> {
    use std::io::Write;
    let mut child = git(repo, &["hash-object", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| could_not_run(repo, e))?;
    child.stdin.take().expect("piped").write_all(bytes)?;
    let out = child.wait_with_output()?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
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
