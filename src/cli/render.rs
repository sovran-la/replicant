//! Text for the terminal. Every function takes the structured reply and
//! returns a string, so the rendering is tested without a terminal; colour is
//! a flag (on when stdout is a tty), red for anything that needs a human.

use crate::daemon::fleet::FleetState;
use crate::daemon::journal::Line;
use crate::daemon::wire::{Enrollment, SubscribeOutcome, UnsubscribeOutcome};
use crate::decide::Decision;
use crate::plan::{LeafStatus, Plan, PlannedUnit};
use crate::timefmt::{ago, iso};
use crate::units::UnitKind;
use std::fmt::Write;

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
        "The next sync removes unsubscribed paths from this machine (through guardrail 1).\n",
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
    let label = if u.decision.is_blocking() {
        st.red(&label)
    } else {
        label
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
    for u in &p.units {
        if matches!(u.decision, Decision::Nothing { .. }) && !verbose {
            continue;
        }
        let _ = writeln!(s, "{}", unit_line(u, st));
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

fn queue(q: &[crate::daemon::queue::QueueView]) -> String {
    if q.is_empty() {
        return "queue: empty\n".into();
    }
    let mut s = String::from("queue:\n");
    for (i, e) in q.iter().enumerate() {
        let _ = writeln!(s, "  {}. {}", i + 1, crate::daemon::queue::describe(e));
    }
    s
}

pub fn fleet(f: &FleetState, st: Style) -> String {
    let mut s = format!(
        "home {} — {} hosts, {} pending, {} in queue\n",
        f.home,
        f.hosts.len(),
        f.pending.len(),
        f.queue.len()
    );
    for h in &f.hosts {
        let contact = match h.last_contact {
            Some(t) => format!("{} ago", ago(f.now.saturating_sub(t))),
            None => "never".into(),
        };
        let contact = if h.stale {
            st.red(&format!("STALE {contact}"))
        } else {
            contact
        };
        let mut counts: Vec<String> = Vec::new();
        let mut blocking = false;
        if let Some(r) = &h.report {
            let mut by: std::collections::BTreeMap<String, usize> = Default::default();
            for u in &r.units {
                let k = match &u.decision {
                    Decision::Nothing { .. } => continue,
                    Decision::Conflict { .. } => "conflict".to_string(),
                    Decision::Quarantined { .. } => "quarantined".to_string(),
                    d => d.label(),
                };
                blocking |= u.decision.is_blocking();
                *by.entry(k).or_default() += 1;
            }
            counts = by.into_iter().map(|(k, n)| format!("{n} {k}")).collect();
            if counts.is_empty() {
                counts.push("in sync".into());
            }
            counts.push(format!(
                "(as of {} {} ago)",
                r.event,
                ago(f.now.saturating_sub(r.at))
            ));
        }
        let pending = if counts.is_empty() {
            "no report yet".to_string()
        } else {
            counts.join(", ")
        };
        let pending = if blocking { st.red(&pending) } else { pending };
        let cap = h
            .cap
            .as_ref()
            .map(|c| format!("  cap: {}", c.join(" ")))
            .unwrap_or_default();
        let q = h
            .queue_position
            .map(|p| format!("  queue #{}", p + 1))
            .unwrap_or_default();
        let _ = writeln!(s, "  {:<12} {:<14} {pending}{cap}{q}", h.name, contact);
        let _ = writeln!(
            s,
            "  {:<12} {}",
            "",
            st.dim(&format!("subscribed: {}", h.paths.join(", ")))
        );
    }
    if !f.pending.is_empty() {
        s.push_str("pending (run `replicant approve`):\n");
        for p in &f.pending {
            let _ = writeln!(
                s,
                "  {:<12} {}  wants: {}",
                p.host,
                p.home_dir.display(),
                p.paths.join(", ")
            );
        }
    }
    for m in &f.markers {
        let _ = writeln!(
            s,
            "{}",
            st.red(&format!(
                "  quarantined: {} (dead {} by {}, since {})",
                m.rel,
                m.direction,
                m.host,
                iso(m.started_at)
            ))
        );
    }
    s.push_str(&queue(&f.queue));
    s
}

pub fn log(lines: &[Line]) -> String {
    lines
        .iter()
        .map(|l| {
            format!(
                "{}  {:<11} {:<12} {:<28} {}\n",
                iso(l.ts),
                l.event,
                l.host,
                l.path,
                l.detail
            )
        })
        .collect()
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
