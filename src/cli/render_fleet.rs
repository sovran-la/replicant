//! The fleet view (`status --fleet`, and `status` on home), the queue, and
//! the journal.

use super::Style;
use crate::daemon::fleet::FleetState;
use crate::daemon::journal::Line;
use crate::decide::Decision;
use crate::timefmt::{ago, iso};
use std::fmt::Write;

pub(super) fn queue(q: &[crate::daemon::queue::QueueView]) -> String {
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
            let as_of = format!(
                " (as of {} {} ago)",
                r.event,
                ago(f.now.saturating_sub(r.at))
            );
            counts = vec![counts.join(", ") + &as_of];
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
