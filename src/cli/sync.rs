//! `replicant sync`: the collect-then-apply pass. PLAN.md, "Collect, then
//! apply".
//!
//! On a terminal: the daemon plans (a dry run), the plan is shown, every
//! deletion awaiting confirmation is asked about one by one, then the whole
//! plan once. Only then is the sync sent, carrying the digest of the plan
//! that was shown, and the daemon applies only a plan with that digest. A
//! `n`, an empty answer, end of input or Ctrl-C anywhere in the questions
//! sends nothing, so nothing is written, not even a marker.
//!
//! `--yes` skips the questions (and the first plan): the daemon plans and
//! applies in one queue entry. It does not confirm deletions; only
//! `--confirm-delete` or a `y` to that deletion's own question does.
//! `--non-interactive`, or no terminal, never asks: without `--yes` it
//! shows the plan and stops (exit 2); every report that is not a clean
//! finish goes to stderr, the one channel the agents daemon keeps.
//!
//! While the daemon applies, each unit is shown as it starts and ends: on
//! stdout at a terminal, on stderr without one (stdout then carries only the
//! report, as before). With `--json`, one object per line: each event, then
//! the report. Ctrl-C here hangs up on the daemon, which stops the running
//! transfer and leaves its unit interrupted, its marker kept.

use super::render::{self, Style};
use crate::Result;
use crate::daemon::wire::{LocalReply, LocalRequest};
use crate::decide::Decision;
use crate::guard::Guard;
use crate::plan::Plan;
use crate::plan::apply::{ProgressEvent, SyncOpts, confirms};
use std::io::{BufRead, Write};

/// What `sync` was asked for, paths already absolutized.
#[derive(Debug, Clone, Default)]
pub struct Args {
    pub dry_run: bool,
    pub yes: bool,
    /// False with `--non-interactive` or without a terminal.
    pub interactive: bool,
    pub confirm_delete: Vec<String>,
    /// `--accept-rollback`: sent with the dry run and the sync alike.
    pub accept_rollback: Vec<String>,
    pub no_wait: bool,
    pub json: bool,
    pub verbose: bool,
    /// This machine's `$HOME`, for a `~/` in `confirm_delete`.
    pub home_dir: std::path::PathBuf,
}

/// The terminal: where questions are read and answers, reports and errors
/// written.
pub struct Term<'a> {
    pub input: &'a mut dyn BufRead,
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
    pub style: Style,
}

/// Ask the daemon; a sync's progress goes to the sink as it arrives.
pub type Call<'c> = &'c dyn Fn(LocalRequest, &mut dyn FnMut(ProgressEvent)) -> Result<LocalReply>;

/// Run `sync`; the return value is the exit code.
pub fn run(a: &Args, t: &mut Term<'_>, call: Call<'_>) -> Result<i32> {
    if a.dry_run {
        let p = plan(a, call)?;
        let code = if p.blocked() { 2 } else { 0 };
        let text = render::plan(&p, a.verbose, t.style);
        return show(a, t, code, text, &p, false);
    }
    if a.yes {
        return send(a, t, a.confirm_delete.clone(), None, true, call);
    }
    if a.json {
        return Err(crate::Error::Config(
            "--json needs --dry-run or --yes: a question cannot be answered in JSON".into(),
        ));
    }
    let p = plan(a, call)?;
    let text = render::plan(&p, a.verbose, t.style);
    let blocked = blockers(&p, &a.confirm_delete, &a.home_dir);
    if !a.interactive {
        let w: &mut dyn Write = &mut *t.err;
        write!(w, "{text}")?;
        writeln!(
            w,
            "{}",
            if blocked {
                "blocked; nothing written"
            } else {
                "not applied: no terminal to confirm on; rerun with --yes to apply"
            }
        )?;
        return Ok(2);
    }
    write!(t.out, "{text}")?;
    if blocked {
        writeln!(t.out, "{}", t.style.red("blocked; nothing written"))?;
        return Ok(2);
    }
    if !p.applies_anything() {
        writeln!(t.out, "nothing to apply")?;
        return Ok(0);
    }
    let mut confirm = a.confirm_delete.clone();
    let pending: Vec<_> = p
        .units
        .iter()
        .filter(|u| matches!(u.decision, Decision::DeletedOn { .. }))
        .filter(|u| !confirm.iter().any(|c| confirms(c, u, &a.home_dir)))
        .collect();
    for u in pending {
        let note = u.deletion_note(&p.me, &p.home).unwrap_or_default();
        if !ask(t, &format!("{}: {note}. Carry it out?", u.rel))? {
            writeln!(t.out, "not confirmed; nothing written")?;
            return Ok(2);
        }
        confirm.push(u.rel.clone());
    }
    if !ask(t, "Apply this plan?")? {
        writeln!(t.out, "not applied; nothing written")?;
        return Ok(1);
    }
    send(a, t, confirm, Some(p.digest()), false, call)
}

fn plan(a: &Args, call: Call<'_>) -> Result<Plan> {
    let req = LocalRequest::DryRun {
        no_wait: a.no_wait,
        accept_rollback: a.accept_rollback.clone(),
    };
    match call(req, &mut |_| {})? {
        LocalReply::Plan(p) => Ok(*p),
        _ => Err(super::unexpected_reply()),
    }
}

/// Whether anything but a deletion awaiting confirmation needs a human. A
/// resume's guardrail 6 is waived by a `--confirm-delete` naming its unit.
fn blockers(p: &Plan, confirm: &[String], home_dir: &std::path::Path) -> bool {
    !p.refused.is_empty()
        || p.units.iter().any(|u| {
            let confirmed = confirm.iter().any(|c| confirms(c, u, home_dir));
            u.refusals
                .iter()
                .any(|r| !(confirmed && r.guard == Guard::ChangedSinceInterrupted))
                || (u.decision.is_blocking() && !matches!(u.decision, Decision::DeletedOn { .. }))
        })
}

/// `[y/N]`: only `y` or `yes` is yes. End of input, or an interrupted read
/// (Ctrl-C), is no.
fn ask(t: &mut Term<'_>, q: &str) -> Result<bool> {
    write!(t.out, "{q} [y/N] ")?;
    t.out.flush()?;
    let mut line = String::new();
    match t.input.read_line(&mut line) {
        Ok(0) | Err(_) => {
            writeln!(t.out)?;
            Ok(false)
        }
        Ok(_) => Ok(matches!(
            line.trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        )),
    }
}

/// Send the sync and show what it did.
fn send(
    a: &Args,
    t: &mut Term<'_>,
    confirm_delete: Vec<String>,
    expect: Option<String>,
    with_plan: bool,
    call: Call<'_>,
) -> Result<i32> {
    let opts = SyncOpts {
        no_wait: a.no_wait,
        confirm_delete,
        expect,
        accept_rollback: a.accept_rollback.clone(),
    };
    let mut streamed = 0;
    let reply = {
        let (json, style) = (a.json, t.style);
        let w: &mut dyn Write = if a.interactive {
            &mut *t.out
        } else {
            &mut *t.err
        };
        let mut lines = render::Lines::new(json, a.interactive, style);
        let mut on = |e: ProgressEvent| {
            if !matches!(e, ProgressEvent::UnitProgress { .. }) {
                streamed += 1;
            }
            let line = lines.line(&e);
            if line.is_empty() {
                return;
            }
            // Best effort: a terminal gone away loses the progress, not the
            // sync; the report's own write reports it.
            let _ = w.write_all(line.as_bytes()).and_then(|()| w.flush());
        };
        call(LocalRequest::Sync { opts }, &mut on)?
    };
    let LocalReply::Synced(r) = reply else {
        return Err(super::unexpected_reply());
    };
    let code = r.outcome.exit_code();
    // Where the units already went by as they ran (stdout at a terminal;
    // stderr otherwise, which is where a report that is not a clean finish
    // goes too), the report does not list them again, nor the plan, unless
    // asked to (--verbose) or nothing ran (the plan says why).
    let seen = streamed > 0 && (a.interactive || code != 0);
    let list = a.verbose || !seen;
    let mut text = String::new();
    if with_plan && list {
        text.push_str(&render::plan(&r.plan, a.verbose, t.style));
    }
    text.push_str(&render::synced(&r, list, t.style));
    show(a, t, code, text, &*r, true)
}

/// Print `text` (or `json`; on one line when it ends a stream of events):
/// to stdout, unless this is a non-interactive run that did not finish
/// cleanly, which reports on stderr.
fn show(
    a: &Args,
    t: &mut Term<'_>,
    code: i32,
    text: String,
    json: &dyn erased_json::Json,
    one_line: bool,
) -> Result<i32> {
    let w: &mut dyn Write = if !a.interactive && code != 0 {
        &mut *t.err
    } else {
        &mut *t.out
    };
    if a.json {
        writeln!(w, "{}", json.json(one_line)?)?;
    } else {
        write!(w, "{text}")?;
    }
    Ok(code)
}

mod erased_json {
    pub trait Json {
        fn json(&self, one_line: bool) -> crate::Result<String>;
    }
    impl<T: serde::Serialize> Json for T {
        fn json(&self, one_line: bool) -> crate::Result<String> {
            match one_line {
                true => serde_json::to_string(self),
                false => serde_json::to_string_pretty(self),
            }
            .map_err(|e| crate::Error::Daemon(e.to_string()))
        }
    }
}

#[cfg(test)]
#[path = "sync_tests.rs"]
mod tests;
