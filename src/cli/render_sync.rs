//! What `sync` does: one line per unit as it runs, then how it ended. Red for
//! anything that needs a human; the end line always says whether anything
//! was written and what the next `sync` will do. And the open markers
//! `status` lists.

use super::Style;
use crate::plan::OpenMarker;
use crate::plan::apply::{Outcome, ProgressEvent, SyncReport, UnitOutcome};
use crate::timefmt::{ago, elapsed};
use std::fmt::Write;

/// `status`'s open markers: each with its age; red once it is older than
/// home's alert age (interrupted), dim before (it may still be running).
pub fn open_markers(markers: &[OpenMarker], me: &str, home: &str, st: Style) -> String {
    if markers.is_empty() {
        return String::new();
    }
    let mut s = String::from("open markers:\n");
    for o in markers {
        let m = &o.marker;
        let next = if m.host == me {
            "the next `replicant sync` resumes it".to_string()
        } else {
            format!(
                "quarantined: it waits for {}'s next sync, or `replicant rollback` on {home}",
                m.host
            )
        };
        let line = format!(
            "  {}  {} by {}, marked on {}, started {} ago; {next}",
            m.rel,
            m.direction,
            m.host,
            o.on,
            ago(o.age_secs)
        );
        let line = match o.alert {
            true => st.red(&format!("{line} (interrupted)")),
            false => st.dim(&format!("{line} (may still be running)")),
        };
        let _ = writeln!(s, "{line}");
    }
    s
}

/// `rollback <id>`: what was put back, and what could not be.
pub fn rolled_back(r: &crate::daemon::wire::RolledBack) -> String {
    let mut s = format!(
        "Rolled back {} ({} by {}): restored {} files, removed {} it created.\n",
        r.id, r.unit, r.host, r.restored, r.removed
    );
    if !r.unbacked.is_empty() {
        let _ = writeln!(
            s,
            "Not backed up, left as pushed: {}",
            r.unbacked.join(", ")
        );
    }
    if !r.unreported.is_empty() {
        let _ = writeln!(
            s,
            "no report ever came for run {:?}: files it created (if any) are still there.",
            r.unreported
        );
    }
    let _ = writeln!(
        s,
        "What the push had written is kept in {}. Baselines are unchanged: {}'s next sync \
         sees home changed and pulls this.",
        r.kept.display(),
        r.host
    );
    s
}

/// `rollback` without an id: every push home still has, newest first.
pub fn pushes(list: &[crate::daemon::home::Push], now: u64) -> String {
    if list.is_empty() {
        return "no pushes recorded\n".into();
    }
    let mut s = String::new();
    for p in list {
        let state = match (p.rolled_back_at, p.finished_at) {
            (Some(_), _) => "rolled back",
            (None, Some(_)) => "finished",
            (None, None) => "interrupted",
        };
        let files: usize = p.runs.iter().map(|r| r.backed_up.len()).sum();
        let _ = writeln!(
            s,
            "{}  {}  {} ago  {state}, {} run(s), {files} files backed up",
            p.id,
            p.unit,
            ago(now.saturating_sub(p.started_at)),
            p.runs.len()
        );
    }
    s
}

/// How the sync ended, and with `list` every unit applied and how long it
/// took (`--verbose`, or wherever the units were not already shown as they
/// ran by [`progress`]).
pub fn synced(r: &SyncReport, list: bool, st: Style) -> String {
    let mut s = String::new();
    if list && !r.applied.is_empty() {
        s.push_str("applied:\n");
        for a in &r.applied {
            let resumed = if a.resumed { ", resumed" } else { "" };
            let backup = match &a.push {
                Some(id) => format!(", backup {id}"),
                None => String::new(),
            };
            let _ = writeln!(
                s,
                "  {:<15} {}  {} files changed, {} deleted{resumed}{backup}, {}",
                a.action,
                a.rel,
                a.files_changed,
                a.deleted,
                elapsed(a.elapsed_ms)
            );
        }
    }
    let n = r.applied.len();
    let units = if n == 1 { "unit" } else { "units" };
    match &r.outcome {
        Outcome::Done if n == 0 => s.push_str("nothing to apply\n"),
        Outcome::Done => {
            let _ = writeln!(s, "done: {n} {units} applied in {}", elapsed(r.elapsed_ms));
        }
        Outcome::Blocked { why } => {
            let _ = writeln!(s, "{}", st.red("blocked; nothing written:"));
            for w in why {
                let _ = writeln!(s, "  {}", st.red(w));
            }
        }
        Outcome::Changed => {
            let _ = writeln!(
                s,
                "{}",
                st.red(
                    "the plan changed after it was shown; nothing written. Run `replicant sync` \
                     again to see the new one."
                )
            );
        }
        Outcome::Interrupted { rel, why, .. } => {
            let _ = writeln!(
                s,
                "{}",
                st.red(&format!(
                    "interrupted at {rel}: {why}. {n} {units} before it are done; its marker \
                     stays and the next `replicant sync` resumes it; nothing after it was touched."
                ))
            );
        }
        Outcome::Failed { rel, why } => {
            let _ = writeln!(
                s,
                "{}",
                st.red(&format!(
                    "failed at {rel}: {why}. {n} {units} before it are done; any marker it left \
                     stays and the next `replicant sync` resumes it."
                ))
            );
        }
    }
    s
}

/// One event of a running sync: a started unit's line without its end
/// (`  [12/155] push  work/x … `), then what a finished one adds to it
/// (`220 files  1.4s`, or why, in red).
pub fn progress(e: &ProgressEvent, st: Style) -> String {
    match e {
        ProgressEvent::UnitStarted {
            rel,
            action,
            index,
            total,
            ..
        } => {
            format!("  [{index}/{total}] {action}  {rel} … ")
        }
        // A unit's counters only redraw its line at a terminal
        // (`render::redraw`); in a stream of lines they are noise.
        ProgressEvent::UnitProgress { .. } => String::new(),
        ProgressEvent::UnitDone {
            action,
            files_changed,
            deleted,
            elapsed_ms,
            push,
            outcome,
            ..
        } => {
            let t = elapsed(*elapsed_ms);
            match outcome {
                UnitOutcome::Applied => {
                    let what = match action.as_str() {
                        "adopt" => "baseline recorded".to_string(),
                        "forget" => "baseline dropped".to_string(),
                        _ => {
                            let files = if *files_changed == 1 { "file" } else { "files" };
                            let mut w = format!("{files_changed} {files}");
                            if *deleted > 0 {
                                let _ = write!(w, ", {deleted} deleted");
                            }
                            w
                        }
                    };
                    let backup = match push {
                        Some(id) => format!("  backup {id}"),
                        None => String::new(),
                    };
                    format!("{what}  {t}{backup}\n")
                }
                UnitOutcome::Interrupted { why } => {
                    format!("{}\n", st.red(&format!("interrupted after {t}: {why}")))
                }
                UnitOutcome::Failed { why } => {
                    format!("{}\n", st.red(&format!("failed after {t}: {why}")))
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "render_sync_tests.rs"]
mod tests;
