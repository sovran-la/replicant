//! The conflict question (PLAN.md, "Conflicts"; rev 22). At a terminal,
//! after the plan is shown, each conflict the person may resolve is asked
//! about by itself: take home's copy, take this machine's, or skip. Taking a
//! copy overwrites the other side's, which is kept in a backup there; skip
//! leaves the unit as it is and lets the rest of the plan go, and the run
//! still ends saying what was left (PLAN.md, "Skip is not neutral").
//!
//! Any other answer is asked again. End of input or Ctrl-C ends the
//! questions with nothing sent, so nothing is written.

use super::Term;
use super::render::{Style, head};
use crate::decide::{Decision, Resolution, Take};
use crate::plan::{Plan, PlannedUnit};
use crate::units::UnitKind;
use std::fmt::Write as _;

/// The person's answers to a plan's conflicts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Answers {
    pub resolve: Vec<Resolution>,
    pub skip: Vec<String>,
}

impl Answers {
    /// Whether these answers take a copy for `rel`.
    pub fn takes(&self, rel: &str) -> bool {
        self.resolve.iter().any(|r| r.rel == rel)
    }
}

/// The conflicts in `p` a person may answer.
pub fn open(p: &Plan) -> Vec<&PlannedUnit> {
    p.units
        .iter()
        .filter(|u| u.resolvable && matches!(u.decision, Decision::Conflict { .. }))
        .collect()
}

/// Ask about every conflict in `p`. `None`: the questions were ended (end
/// of input, Ctrl-C); nothing may be sent.
pub fn ask_all(t: &mut Term<'_>, p: &Plan) -> std::io::Result<Option<Answers>> {
    let mut a = Answers::default();
    for u in open(p) {
        match ask(t, p, u)? {
            None => return Ok(None),
            Some(None) => a.skip.push(u.rel.clone()),
            Some(Some(take)) => a.resolve.push(Resolution {
                rel: u.rel.clone(),
                take,
            }),
        }
    }
    Ok(Some(a))
}

/// One conflict's question. `None`: ended; `Some(None)`: skipped.
fn ask(t: &mut Term<'_>, p: &Plan, u: &PlannedUnit) -> std::io::Result<Option<Option<Take>>> {
    write!(t.out, "{}", question(p, u, t.style))?;
    loop {
        write!(t.out, "Which? [1/2/s] ")?;
        t.out.flush()?;
        let mut line = String::new();
        match t.input.read_line(&mut line) {
            Ok(0) | Err(_) => {
                writeln!(t.out)?;
                return Ok(None);
            }
            Ok(_) => {}
        }
        match parse(line.trim(), &p.me, &p.home) {
            Some(answer) => return Ok(Some(answer)),
            None => writeln!(
                t.out,
                "answer 1 ({}'s copy), 2 ({}'s copy) or s (skip)",
                p.home, p.me
            )?,
        }
    }
}

/// An answer: `1` or home's name takes home's copy, `2` or this machine's
/// name takes this one's, `s` or `skip` skips. Case is ignored.
pub fn parse(answer: &str, me: &str, home: &str) -> Option<Option<Take>> {
    let a = answer.to_ascii_lowercase();
    match a.as_str() {
        "s" | "skip" => Some(None),
        "1" => Some(Some(Take::Home)),
        "2" => Some(Some(Take::Leaf)),
        _ if a == home.to_ascii_lowercase() => Some(Some(Take::Home)),
        _ if a == me.to_ascii_lowercase() => Some(Some(Take::Leaf)),
        _ => None,
    }
}

/// What the question shows: each side's copy, and the three choices.
pub fn question(p: &Plan, u: &PlannedUnit, st: Style) -> String {
    let (me, home) = (p.me.as_str(), p.home.as_str());
    let w = me.len().max(home.len());
    let mut s = String::new();
    let what = match u.kind {
        UnitKind::Repo => "Repo conflict",
        UnitKind::Loose => "Conflict",
    };
    let why = match &u.decision {
        Decision::Conflict { why } => why.as_str(),
        _ => "",
    };
    let _ = writeln!(s, "{}", st.red(&format!("{what}: {} ({why})", u.rel)));
    let side = |head_: &Option<String>, dirty: usize, files: u64| match u.kind {
        UnitKind::Repo => format!(
            "{} @ {}",
            head(head_),
            match dirty {
                0 => "clean".to_string(),
                n => format!("{n} dirty"),
            }
        ),
        UnitKind::Loose => format!("{files} {}", if files == 1 { "file" } else { "files" }),
    };
    let _ = writeln!(
        s,
        "  {home:<w$}  {}",
        side(&u.home_head, u.home_dirty, u.home_files)
    );
    let _ = writeln!(
        s,
        "  {me:<w$}  {}",
        side(&u.leaf_head, u.leaf_dirty, u.leaf_files)
    );
    let _ = writeln!(
        s,
        "  1) use {home}'s copy  (pull: {me}'s is kept in a backup here)"
    );
    let _ = writeln!(
        s,
        "  2) use {me}'s copy  (push: {home}'s is kept in its push backup)"
    );
    let _ = writeln!(s, "  s) skip: leave it as it is, and sync the rest");
    s
}

/// What a run that skipped conflicts ends with: red, one line per path.
pub fn unresolved(skip: &[String], st: Style) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "{}",
        st.red(&format!(
            "{} conflict{} left unresolved; the next sync asks again:",
            skip.len(),
            if skip.len() == 1 { "" } else { "s" }
        ))
    );
    for rel in skip {
        let _ = writeln!(s, "  {}", st.red(rel));
    }
    s
}

#[cfg(test)]
#[path = "sync_conflicts_tests.rs"]
mod tests;
