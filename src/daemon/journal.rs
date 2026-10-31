//! The append-only journal. PLAN.md, "Journal".
//!
//! One event per line, tab-separated so a path or detail with spaces cannot
//! shift a column:
//!
//! ```text
//! 2026-10-31T19:04:11Z ⇥ dry-run ⇥ birchbook ⇥ work/tundra ⇥ push 3 repos 41 files 0 del
//! ```
//!
//! (⇥ is a tab.) Home journals everything. A leaf appends the lines home returns about it,
//! so its own history survives home being unreachable. v0.1 journals
//! subscribe, unsubscribe, pending, approve, status, dry-run and lease expiry;
//! the full event set is v0.2.

use crate::paths;
use crate::timefmt;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    pub ts: u64,
    pub event: String,
    pub host: String,
    /// `-` when the event is about no particular path.
    pub path: String,
    pub detail: String,
}

impl Line {
    pub fn new(event: &str, host: &str, path: &str, detail: impl Into<String>) -> Self {
        Self {
            ts: crate::now_secs(),
            event: event.into(),
            host: host.into(),
            path: if path.is_empty() {
                "-".into()
            } else {
                path.into()
            },
            detail: detail.into(),
        }
    }

    pub fn render(&self) -> String {
        let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
        format!(
            "{}\t{}\t{}\t{}\t{}",
            timefmt::iso(self.ts),
            clean(&self.event),
            clean(&self.host),
            clean(&self.path),
            clean(&self.detail)
        )
    }

    pub fn parse(text: &str) -> Option<Self> {
        let mut f = text.splitn(5, '\t');
        Some(Self {
            ts: timefmt::parse_iso(f.next()?)?,
            event: f.next()?.into(),
            host: f.next()?.into(),
            path: f.next()?.into(),
            detail: f.next().unwrap_or("").into(),
        })
    }
}

/// What `replicant log` asks for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filter {
    pub host: Option<String>,
    /// A normalized path; matches events on it, beneath it, or above it.
    pub path: Option<String>,
    pub since: Option<u64>,
}

impl Filter {
    pub fn admits(&self, l: &Line) -> bool {
        self.host.as_ref().is_none_or(|h| *h == l.host)
            && self.since.is_none_or(|s| l.ts >= s)
            && self
                .path
                .as_ref()
                .is_none_or(|p| paths::is_within(&l.path, p) || paths::is_within(p, &l.path))
    }
}

#[derive(Debug)]
pub struct Journal {
    path: PathBuf,
    /// Serializes appends from this process's threads; O_APPEND keeps a
    /// single short write atomic against other processes.
    lock: Mutex<()>,
}

impl Journal {
    pub fn at(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    /// Append one event, returning the line as written.
    pub fn append(&self, line: &Line) -> Result<String> {
        let text = line.render();
        self.append_raw(&text)?;
        Ok(text)
    }

    /// Append a line already rendered elsewhere (a leaf copying home's).
    pub fn append_raw(&self, text: &str) -> Result<()> {
        if Line::parse(text).is_none() {
            return Err(Error::Daemon(format!(
                "refusing to journal a malformed line: {text:?}"
            )));
        }
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        f.write_all(format!("{text}\n").as_bytes())?;
        Ok(())
    }

    /// Every line the filter admits, oldest first. Unparseable lines are
    /// skipped rather than failing the read: the journal is evidence, and
    /// one torn line should not hide the rest.
    pub fn read(&self, filter: &Filter) -> Result<Vec<Line>> {
        let f = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for l in std::io::BufReader::new(f).lines() {
            if let Some(line) = Line::parse(&l?)
                && filter.admits(&line)
            {
                out.push(line);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;
