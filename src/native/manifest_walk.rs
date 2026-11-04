//! The parallel walk under a unit's root.

use super::{Entry, Manifest, is_special};
use crate::exclude::Excludes;
use crate::native::is_temp;
use crate::{Error, Result};
use ignore::{DirEntry, ParallelVisitor, ParallelVisitorBuilder, WalkBuilder, WalkState};
use std::fs;
use std::path::Path;
use std::sync::mpsc::{Sender, channel};

#[derive(Default)]
struct Found {
    entries: Vec<Entry>,
    temps: Vec<String>,
    skipped: Vec<String>,
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
        // Not UTF-8: skipped and not descended, as discovery does.
        let Some(rel) = abs.strip_prefix(self.base).ok().and_then(Path::to_str) else {
            return Ok(WalkState::Skip);
        };
        let meta = fs::symlink_metadata(abs)?;
        let name = rel.rsplit('/').next().unwrap_or(rel);
        if is_temp(name) && !meta.is_dir() && !is_special(&meta) {
            self.found.temps.push(rel.to_string());
            return Ok(WalkState::Skip);
        }
        if self.ex.matches(rel, meta.is_dir()) {
            return Ok(WalkState::Skip);
        }
        if is_special(&meta) {
            self.found.skipped.push(rel.to_string());
            return Ok(WalkState::Skip);
        }
        self.found.entries.push(Entry::of(abs, rel, &meta)?);
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

/// Every entry under the directory `base`, unsorted.
pub(super) fn walk_dir(base: &Path, ex: &Excludes) -> Result<Manifest> {
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
    let mut m = Manifest::default();
    let mut error = None;
    for f in rx {
        m.entries.extend(f.entries);
        m.temps.extend(f.temps);
        m.skipped.extend(f.skipped);
        error = error.or(f.error);
    }
    match error {
        Some(e) => Err(e),
        None => Ok(m),
    }
}
