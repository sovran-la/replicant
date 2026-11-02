//! One side's units, hashed: [`snapshot`] turns a [`Discovered`] and the
//! union of both sides' repos into [`SideUnit`]s.

use super::{Discovered, SideUnit, SubSpec, UnitKind, excludes, first_component};
use crate::Result;
use crate::exclude::Excludes;
use crate::paths;
use crate::repos;
use crate::state::{self, UnitState};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// This side's units, against the union of both sides' repos.
pub fn snapshot(
    found: &Discovered,
    union_repos: &[String],
    case_check: bool,
) -> Result<Vec<SideUnit>> {
    snapshot_timed(found, union_repos, case_check).map(|(u, _)| u)
}

/// How many of the slowest units `snapshot_timed` names.
const SLOWEST: usize = 5;

/// A unit's home-relative path and how long its hash took, in ms.
pub type UnitTiming = (String, u128);

/// [`snapshot`], plus the slowest units and their wall time in ms.
pub fn snapshot_timed(
    found: &Discovered,
    union_repos: &[String],
    case_check: bool,
) -> Result<(Vec<SideUnit>, Vec<UnitTiming>)> {
    struct Job<'a> {
        spec: &'a SubSpec,
        abs: PathBuf,
        rel: String,
        kind: UnitKind,
        loose: Vec<repos::Entry>,
        is_file: bool,
        excludes: Excludes,
    }
    let mut jobs = Vec::new();
    for (spec, sub_abs, d) in &found.subs {
        let repos_here: Vec<&String> = union_repos
            .iter()
            .filter(|r| paths::is_within(r, &spec.rel))
            .collect();
        for r in &repos_here {
            let inner = paths::relative_to(r, &spec.rel).unwrap_or("");
            let ex = excludes(spec, r, UnitKind::Repo, union_repos, &[])?;
            jobs.push(Job {
                spec,
                abs: if inner.is_empty() {
                    sub_abs.clone()
                } else {
                    sub_abs.join(inner)
                },
                rel: (*r).clone(),
                kind: UnitKind::Repo,
                loose: Vec::new(),
                is_file: false,
                excludes: ex,
            });
        }
        if repos_here.iter().any(|r| **r == spec.rel) {
            continue;
        }
        // Blocked paths, made relative to the subscription so each loose entry
        // is tested as-is, and bucketed by first component so an entry is
        // compared only with the few that could contain it. The plain scan
        // was 70k entries × 154 repos on thistledown: two seconds, serial.
        let mut blocked: HashMap<&str, Vec<&str>> = HashMap::new();
        for b in repos_here
            .iter()
            .map(|r| r.as_str())
            .chain(spec.except.iter().map(String::as_str))
        {
            if let Some(inner) = paths::relative_to(b, &spec.rel).filter(|x| !x.is_empty()) {
                blocked
                    .entry(first_component(inner))
                    .or_default()
                    .push(inner);
            }
        }
        let loose = d
            .loose
            .iter()
            .filter(|e| {
                blocked
                    .get(first_component(&e.rel))
                    .is_none_or(|bs| !bs.iter().any(|b| paths::is_within(&e.rel, b)))
            })
            .cloned()
            .collect();
        let is_file = d.loose.len() == 1 && d.loose[0].rel.is_empty();
        let ex = excludes(spec, &spec.rel, UnitKind::Loose, union_repos, &[])?;
        jobs.push(Job {
            spec,
            abs: sub_abs.clone(),
            rel: spec.rel.clone(),
            kind: UnitKind::Loose,
            loose,
            is_file,
            excludes: ex,
        });
    }

    let results = state::parallel_map(&jobs, state::workers(), |j| -> Result<(SideUnit, u128)> {
        let t = std::time::Instant::now();
        let mut u = SideUnit {
            rel: j.rel.clone(),
            sub: j.spec.rel.clone(),
            kind: j.kind,
            state: UnitState::Empty,
            head: None,
            dirty: Vec::new(),
            locks: Vec::new(),
            collisions: Vec::new(),
            is_file: j.is_file,
            files: 0,
            ignored: Vec::new(),
        };
        let ex = match j.kind {
            UnitKind::Repo => repo_side(&j.abs, &j.excludes, &mut u)?,
            UnitKind::Loose => {
                u.state = state::loose(&j.loose);
                u.files = state::loose_files(&j.loose);
                j.excludes.clone()
            }
        };
        let hash_ms = t.elapsed().as_millis();
        if case_check && !j.is_file {
            let names = repos::all_names(&j.abs, &ex)?;
            u.collisions = repos::case_collisions(names.iter().map(String::as_str));
        }
        let total = t.elapsed().as_millis();
        if total >= 100 {
            eprintln!(
                "  unit {}: hash {hash_ms}ms, case walk {}ms",
                j.rel,
                total - hash_ms
            );
        }
        Ok((u, total))
    });
    let timed: Vec<(SideUnit, u128)> = results.into_iter().collect::<Result<_>>()?;
    let mut slowest: Vec<(String, u128)> =
        timed.iter().map(|(u, ms)| (u.rel.clone(), *ms)).collect();
    slowest.sort_by(|a, b| b.1.cmp(&a.1));
    slowest.truncate(SLOWEST);
    Ok((timed.into_iter().map(|(u, _)| u).collect(), slowest))
}

/// A real repo's facts with its git-ignored paths taken out: the facts, the
/// ignored list (minus what `ex` already drops), and `ex` with that list
/// added, which is what the case walk and rsync must use for this side.
pub(crate) fn repo_facts(
    abs: &Path,
    ex: &Excludes,
) -> Result<(state::RepoFacts, Vec<String>, Excludes)> {
    let ignored: Vec<String> = repos::ignored(abs)?
        .into_iter()
        .filter(|p| !ex.path_excluded(p, p.ends_with('/')))
        .collect();
    let ex = super::with_ignored(ex.clone(), &ignored);
    Ok((state::repo(abs, &ex)?, ignored, ex))
}

/// Fill a repo unit: a real repo here, a plain directory where the other side
/// has a repo, or nothing. Returns the excludes the rest of this side's look
/// (the case walk) must use.
fn repo_side(abs: &Path, ex: &Excludes, u: &mut SideUnit) -> Result<Excludes> {
    match repos::git_marker(abs) {
        repos::GitMarker::Repo => {
            let (f, ignored, ex) = repo_facts(abs, ex)?;
            u.state = f.state;
            u.head = Some(f.head);
            u.dirty = f.dirty;
            u.locks = f.locks;
            u.files = f.files;
            u.ignored = ignored;
            Ok(ex)
        }
        _ => {
            let d = repos::discover(abs, ex)?;
            // Not a repo on this side: hash what is there as loose content, so
            // it can never equal a repo's hash.
            u.state = match state::loose(&d.loose) {
                UnitState::Present(h) => UnitState::Present(format!("notrepo-{h}")),
                UnitState::Empty => UnitState::Empty,
            };
            u.files = state::loose_files(&d.loose);
            Ok(ex.clone())
        }
    }
}
