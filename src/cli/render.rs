//! Text for the terminal. Every function takes the structured reply and
//! returns a string, so the rendering is tested without a terminal; colour is
//! a flag (on when stdout is a tty), red for anything that needs a human.

use crate::daemon::wire::{Enrollment, SubscribeOutcome, UnsubscribeOutcome};
use crate::decide::Decision;
use crate::plan::{LeafStatus, Plan, PlannedUnit, Tone};
use crate::units::UnitKind;
use std::fmt::Write;

#[path = "render_fleet.rs"]
mod fleet_view;
use fleet_view::queue;
pub use fleet_view::{fleet, log, revoked};

/// How many diff lines a unit shows without `--verbose`.
pub const DIFF_PREVIEW: usize = 12;

#[derive(Debug, Clone, Copy)]
pub struct Style {
    pub color: bool,
}

impl Style {
    fn red(self, s: &str) -> String {
        if self.color {
            format!("\x1b[31m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
    fn dim(self, s: &str) -> String {
        if self.color {
            format!("\x1b[2m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
}

pub fn subscribed(o: &SubscribeOutcome) -> String {
    let mut s = format!("requested: {}\n", o.requested.join(", "));
    if !o.absorbed.is_empty() {
        let _ = writeln!(
            s,
            "absorbed (covered by a broader path): {}",
            o.absorbed.join(", ")
        );
    }
    if o.enrolled {
        let _ = writeln!(s, "subscribed on {}: {}", o.home, o.paths.join(", "));
    } else {
        let _ = writeln!(
            s,
            "pending on {}: {}\nThis machine is not enrolled yet. On {} run `replicant approve`; \
             nothing is served until then.",
            o.home,
            o.paths.join(", "),
            o.home
        );
    }
    s
}

pub fn unsubscribed(o: &UnsubscribeOutcome) -> String {
    let mut s = String::new();
    if !o.removed.is_empty() {
        let _ = writeln!(s, "removed: {}", o.removed.join(", "));
    }
    if !o.carved.is_empty() {
        let _ = writeln!(s, "carved out: {}", o.carved.join(", "));
    }
    let _ = writeln!(
        s,
        "now subscribed: {}",
        if o.paths.is_empty() {
            "nothing".into()
        } else {
            o.paths.join(", ")
        }
    );
    if !o.except.is_empty() {
        let _ = writeln!(s, "except: {}", o.except.join(", "));
    }
    s.push_str(
        "The next sync removes unsubscribed paths from this machine, if nothing changed there \
         since the last sync (and through guardrail 1).\n",
    );
    s
}

fn head(h: &Option<String>) -> String {
    match h.as_deref() {
        Some(h) => h
            .strip_prefix("ref: refs/heads/")
            .unwrap_or(&h[..h.len().min(12)])
            .to_string(),
        None => "-".into(),
    }
}

fn unit_line(u: &PlannedUnit, st: Style) -> String {
    let label = u.decision.label();
    let label = match (&u.decision, u.decision.is_blocking()) {
        (_, true) | (Decision::Orphaned { changed: true, .. }, _) => st.red(&label),
        (Decision::Orphaned { .. }, _) => st.dim(&label),
        _ => label,
    };
    let kind = match u.kind {
        UnitKind::Repo => "repo",
        UnitKind::Loose => "loose",
    };
    format!(
        "  {label:<12} {:<40} {kind:<5}  home {} {}  leaf {} {}",
        u.rel,
        head(&u.home_head),
        u.home.short(),
        head(&u.leaf_head),
        u.leaf.short()
    )
}

pub fn plan(p: &Plan, verbose: bool, st: Style) -> String {
    let mut s = String::new();
    for line in p.summary() {
        let _ = writeln!(s, "{line}");
    }
    for r in &p.refused {
        let _ = writeln!(
            s,
            "{}",
            st.red(&format!("{}: {}", r.headline(), r.detail.join("; ")))
        );
    }
    for r in &p.orphaned {
        let _ = writeln!(
            s,
            "{}",
            st.dim(&format!(
                "not served: {}: {}",
                r.headline(),
                r.detail.join("; ")
            ))
        );
    }
    for u in &p.units {
        if matches!(u.decision, Decision::Nothing { .. }) && !verbose {
            continue;
        }
        let _ = writeln!(s, "{}", unit_line(u, st));
        if let Some(note) = u.deletion_note(&p.me, &p.home) {
            let _ = writeln!(s, "    {}", st.red(&note));
        }
        match u.unserved_note() {
            Some((note, Tone::Alarm)) => {
                let _ = writeln!(s, "    {}", st.red(&note));
            }
            Some((note, Tone::Quiet)) => {
                let _ = writeln!(s, "    {}", st.dim(&note));
            }
            None => {}
        }
        for r in &u.refusals {
            let _ = writeln!(s, "    {}", st.red(&format!("REFUSED {}", r.headline())));
            for d in &r.detail {
                let _ = writeln!(s, "      {}", st.red(d));
            }
        }
        if let Some(d) = &u.diff {
            let _ = writeln!(
                s,
                "    {} files changed, {} deleted",
                d.files_changed, d.deleted
            );
            let shown = if verbose {
                d.changes.len()
            } else {
                DIFF_PREVIEW
            };
            for c in d.changes.iter().take(shown) {
                let _ = writeln!(s, "      {c}");
            }
            if d.changes.len() > shown {
                let _ = writeln!(
                    s,
                    "      {}",
                    st.dim(&format!("… {} more (--verbose)", d.changes.len() - shown))
                );
            }
            for n in &d.notes {
                let _ = writeln!(s, "      {}", st.dim(n));
            }
        }
    }
    let nothing = p
        .units
        .iter()
        .filter(|u| matches!(u.decision, Decision::Nothing { .. }))
        .count();
    if nothing > 0 && !verbose {
        let _ = writeln!(
            s,
            "  {}",
            st.dim(&format!("{nothing} units in sync (--verbose to list)"))
        );
    }
    let t = &p.timings;
    let _ = writeln!(
        s,
        "{}",
        st.dim(&format!(
            "timing: total {}ms (discover {}ms, queue {}ms, home state {}ms, leaf state {}ms, rsync {}ms)",
            t.total_ms, t.discover_ms, t.queue_ms, t.home_state_ms, t.leaf_state_ms, t.rsync_ms
        ))
    );
    if p.dry_run {
        let _ = writeln!(s, "{}", st.dim("dry run: nothing was written"));
    }
    s
}

pub fn leaf_status(l: &LeafStatus, st: Style) -> String {
    let mut s = String::new();
    let cap = match &l.cap {
        Some(c) => format!("cap: {}", c.join(", ")),
        None => "no cap".into(),
    };
    match l.enrollment {
        Enrollment::Enrolled => {
            let _ = writeln!(s, "{} → home {} (enrolled; {cap})", l.me, l.home);
        }
        Enrollment::Pending => {
            let _ = writeln!(
                s,
                "{} → home {}: {} (asked for {}). Run `replicant approve` on {}.",
                l.me,
                l.home,
                st.red("pending approval"),
                l.pending_paths.join(", "),
                l.home
            );
        }
        Enrollment::Unknown => {
            let _ = writeln!(
                s,
                "{} → home {}: not enrolled. Run `replicant subscribe`.",
                l.me, l.home
            );
        }
    }
    if !l.paths.is_empty() {
        let _ = writeln!(s, "subscriptions: {}", l.paths.join(", "));
    }
    if !l.except.is_empty() {
        let _ = writeln!(s, "except: {}", l.except.join(", "));
    }
    for (k, v) in &l.as_local {
        let _ = writeln!(s, "  --as {k} → {}", v.display());
    }
    if let Some(p) = &l.plan {
        s.push_str(&plan(p, false, st));
    }
    s.push_str(&queue(&l.queue));
    s
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
