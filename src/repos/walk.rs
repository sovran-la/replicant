//! The parallel walk behind [`super::discover`].
//!
//! `ignore`'s parallel walker, the one the sovran filesystem crate uses, with
//! every ignore source switched off: replicant has to see what rsync moves,
//! hidden and git-ignored files included. (Git-ignored files stop syncing
//! only inside a repo, which this walk never descends: a repo unit's ignored
//! paths come from [`super::ignored`] and reach every walk of it, and rsync,
//! as excludes.) Each walker thread keeps its own
//! findings and sends them down a channel when the walk finishes; the caller
//! sorts, so the result does not depend on thread scheduling.

use super::{Discovery, Entry, GitMarker, entry, git_marker};
use crate::exclude::Excludes;
use crate::{Error, Result};
use ignore::{DirEntry, ParallelVisitor, ParallelVisitorBuilder, WalkBuilder, WalkState};
use std::fs;
use std::path::Path;
use std::sync::mpsc::{Sender, channel};

/// What one walker thread found.
#[derive(Default)]
struct Found {
    repos: Vec<String>,
    worktrees: Vec<String>,
    loose: Vec<Entry>,
    error: Option<Error>,
}

struct Visitor<'a> {
    base: &'a Path,
    ex: &'a Excludes,
    found: Found,
    tx: Sender<Found>,
}

impl Drop for Visitor<'_> {
    fn drop(&mut self) {
        // The receiver outlives every visitor: `walk` drains it only after
        // `visit` has returned and dropped them all.
        let _ = self.tx.send(std::mem::take(&mut self.found));
    }
}

impl ParallelVisitor for Visitor<'_> {
    fn visit(&mut self, item: std::result::Result<DirEntry, ignore::Error>) -> WalkState {
        match self.step(item) {
            Ok(state) => state,
            Err(e) => {
                self.found.error.get_or_insert(e);
                WalkState::Quit
            }
        }
    }
}

impl Visitor<'_> {
    fn step(&mut self, item: std::result::Result<DirEntry, ignore::Error>) -> Result<WalkState> {
        let item = item.map_err(|e| match e.into_io_error() {
            Some(io) => Error::Io(io),
            None => Error::Io(std::io::Error::other("directory walk failed")),
        })?;
        if item.depth() == 0 {
            return Ok(WalkState::Continue);
        }
        let abs = item.path();
        // A name that is not UTF-8 is skipped, and not descended, as the
        // serial walk did: no relative path in this crate can hold it.
        let Some(rel) = abs.strip_prefix(self.base).ok().and_then(Path::to_str) else {
            return Ok(WalkState::Skip);
        };
        let meta = fs::symlink_metadata(abs)?;
        if self.ex.matches(rel, meta.is_dir()) {
            return Ok(WalkState::Skip);
        }
        if meta.is_dir() {
            // A repo or worktree directory is its own unit (or machine-local),
            // not a loose entry, and is never descended.
            match git_marker(abs) {
                GitMarker::Repo => {
                    self.found.repos.push(rel.to_string());
                    return Ok(WalkState::Skip);
                }
                GitMarker::Worktree => {
                    self.found.worktrees.push(rel.to_string());
                    return Ok(WalkState::Skip);
                }
                GitMarker::None | GitMarker::OtherFile => {}
            }
        }
        self.found.loose.push(entry(abs, rel, &meta)?);
        Ok(WalkState::Continue)
    }
}

struct Builder<'a> {
    base: &'a Path,
    ex: &'a Excludes,
    tx: Sender<Found>,
}

impl<'s> ParallelVisitorBuilder<'s> for Builder<'s> {
    fn build(&mut self) -> Box<dyn ParallelVisitor + 's> {
        Box::new(Visitor {
            base: self.base,
            ex: self.ex,
            found: Found::default(),
            tx: self.tx.clone(),
        })
    }
}

/// Walk the directory `base` (not itself a repo or worktree) into `d`.
pub(super) fn walk(base: &Path, ex: &Excludes, d: &mut Discovery) -> Result<()> {
    let (tx, rx) = channel();
    let mut builder = Builder { base, ex, tx };
    WalkBuilder::new(base)
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .ignore(false)
        .parents(false)
        .require_git(false)
        .follow_links(false)
        .threads(crate::state::workers())
        .build_parallel()
        .visit(&mut builder);
    drop(builder);
    let mut error = None;
    for f in rx {
        d.repos.extend(f.repos);
        d.worktrees.extend(f.worktrees);
        d.loose.extend(f.loose);
        error = error.or(f.error);
    }
    match error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

#[cfg(test)]
#[path = "walk_tests.rs"]
mod tests;
