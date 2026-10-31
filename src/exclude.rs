//! Exclude patterns, matched the way rsync matches them.
//!
//! The same list is handed to rsync as `--exclude=` and used by replicant's own
//! walkers (discovery, the manifest, porcelain filtering), so both must agree on
//! what a pattern means. The subset implemented is rsync's:
//!
//! - no `/` (other than a trailing one): matches the final path component
//! - an inner `/`: matches the end of the path at a component boundary
//! - a leading `/`: anchored to the transfer root (only generated internally;
//!   fleet.toml refuses anchored patterns, see [`crate::config::fleet`])
//! - a trailing `/`: directories only
//! - `*` any run of non-`/`, `**` any run at all, `?` one non-`/`, `[...]` a class
//!
//! rsync does not descend into an excluded directory, so a path is excluded if
//! it or any ancestor matches; [`Excludes::path_excluded`] checks that.

/// What is never synced, whatever the root says. PLAN.md, "Exclusions".
///
/// `target/`, `node_modules/` and friends are per-root (fleet.toml's `exclude`)
/// rather than built in, because `bin/` and `.build/` are real source in some
/// trees and the plan's example config lists them per root.
pub const BUILTIN: &[&str] = &[".git/**.lock", ".git/worktrees/"];

#[derive(Debug, Clone, PartialEq, Eq)]
struct Pattern {
    raw: String,
    glob: String,
    anchored: bool,
    dir_only: bool,
    has_slash: bool,
}

/// A list of patterns, applied relative to one transfer root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Excludes {
    patterns: Vec<Pattern>,
}

impl Excludes {
    pub fn new<S: AsRef<str>>(patterns: &[S]) -> Self {
        let mut e = Self::default();
        for p in patterns {
            e.push(p.as_ref());
        }
        e
    }

    /// The built-ins plus a root's own excludes.
    pub fn for_root(root_excludes: &[String]) -> Self {
        let mut e = Self::new(BUILTIN);
        for p in root_excludes {
            e.push(p);
        }
        e
    }

    pub fn push(&mut self, raw: &str) {
        if raw.is_empty() || self.patterns.iter().any(|p| p.raw == raw) {
            return;
        }
        let anchored = raw.starts_with('/');
        let dir_only = raw.ends_with('/');
        let glob = raw
            .trim_start_matches('/')
            .trim_end_matches('/')
            .to_string();
        let has_slash = glob.contains('/');
        self.patterns.push(Pattern {
            raw: raw.to_string(),
            glob,
            anchored,
            dir_only,
            has_slash,
        });
    }

    /// Exclude one exact path relative to the transfer root (a repo dir, a
    /// worktree checkout, an `except`). Glob characters in it are escaped.
    pub fn push_exact(&mut self, rel: &str, is_dir: bool) {
        let mut s = String::from("/");
        for c in rel.chars() {
            if matches!(c, '*' | '?' | '[' | '\\') {
                s.push('\\');
            }
            s.push(c);
        }
        if is_dir {
            s.push('/');
        }
        self.push(&s);
    }

    /// Whether this one entry matches, ignoring its ancestors.
    pub fn matches(&self, rel: &str, is_dir: bool) -> bool {
        self.patterns.iter().any(|p| p.matches(rel, is_dir))
    }

    /// Whether a path is excluded: it, or any directory above it, matches.
    pub fn path_excluded(&self, rel: &str, is_dir: bool) -> bool {
        let rel = rel.trim_end_matches('/');
        let mut end = 0;
        while let Some(i) = rel[end..].find('/') {
            if self.matches(&rel[..end + i], true) {
                return true;
            }
            end += i + 1;
        }
        self.matches(rel, is_dir)
    }

    /// The patterns as rsync arguments.
    pub fn rsync_args(&self) -> Vec<String> {
        self.patterns
            .iter()
            .map(|p| format!("--exclude={}", p.raw))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }
}

impl Pattern {
    fn matches(&self, rel: &str, is_dir: bool) -> bool {
        if self.dir_only && !is_dir {
            return false;
        }
        let g = self.glob.as_bytes();
        if self.anchored {
            return glob(g, rel.as_bytes());
        }
        if !self.has_slash {
            let last = rel.rsplit('/').next().unwrap_or(rel);
            return glob(g, last.as_bytes());
        }
        // Any suffix that starts at a component boundary.
        let b = rel.as_bytes();
        if glob(g, b) {
            return true;
        }
        b.iter()
            .enumerate()
            .filter(|(_, c)| **c == b'/')
            .any(|(i, _)| glob(g, &b[i + 1..]))
    }
}

/// rsync-flavoured wildcard match of a whole string.
fn glob(p: &[u8], s: &[u8]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some(b'*') if p.get(1) == Some(&b'*') => {
            let rest = &p[2..];
            (0..=s.len()).any(|i| glob(rest, &s[i..]))
        }
        Some(b'*') => {
            let rest = &p[1..];
            for i in 0..=s.len() {
                if glob(rest, &s[i..]) {
                    return true;
                }
                if s.get(i) == Some(&b'/') {
                    break;
                }
            }
            false
        }
        Some(b'?') => matches!(s.first(), Some(c) if *c != b'/') && glob(&p[1..], &s[1..]),
        Some(b'[') => match (class(&p[1..]), s.first()) {
            (Some((set, negated, used)), Some(c)) if *c != b'/' => {
                set.contains(c) != negated && glob(&p[1 + used..], &s[1..])
            }
            (None, Some(c)) => *c == b'[' && glob(&p[1..], &s[1..]),
            _ => false,
        },
        Some(b'\\') if p.len() > 1 => s.first() == Some(&p[1]) && glob(&p[2..], &s[1..]),
        Some(c) => s.first() == Some(c) && glob(&p[1..], &s[1..]),
    }
}

/// Parse a `[...]` class body. Returns the set, whether it is negated, and how
/// many pattern bytes it used including the closing `]`.
fn class(p: &[u8]) -> Option<(Vec<u8>, bool, usize)> {
    let (negated, mut i) = match p.first() {
        Some(b'!') | Some(b'^') => (true, 1),
        _ => (false, 0),
    };
    let mut set = Vec::new();
    let start = i;
    while i < p.len() {
        if p[i] == b']' && i > start {
            return Some((set, negated, i + 1));
        }
        if i + 2 < p.len() && p[i + 1] == b'-' && p[i + 2] != b']' {
            set.extend(p[i]..=p[i + 2]);
            i += 3;
        } else {
            set.push(p[i]);
            i += 1;
        }
    }
    None
}

#[cfg(test)]
#[path = "exclude_tests.rs"]
mod tests;
