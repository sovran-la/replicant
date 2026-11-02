//! The fleet view (`status --fleet`, and `status` on home), the queue, the
//! journal, and `host revoke`.

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
        let last = match &h.last_sync {
            None => st.dim("last sync: never"),
            Some(l) => {
                let line = format!(
                    "last sync: {} ago, {}",
                    ago(f.now.saturating_sub(l.at)),
                    l.summary
                );
                if l.ok { line } else { st.red(&line) }
            }
        };
        let _ = writeln!(s, "  {:<12} {last}", "");
        for m in &h.markers {
            let _ = writeln!(
                s,
                "  {:<12} {}",
                "",
                st.red(&format!(
                    "open marker: {} {} on {}, started {} ago",
                    m.direction,
                    m.rel,
                    h.name,
                    ago(f.now.saturating_sub(m.started_at))
                ))
            );
        }
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
                "  quarantined: {} (dead {} by {}, since {}, {} ago{})",
                m.rel,
                m.direction,
                m.host,
                iso(m.started_at),
                ago(f.now.saturating_sub(m.started_at)),
                m.push
                    .as_ref()
                    .map(|p| format!("; `replicant rollback {p}` undoes it"))
                    .unwrap_or_default()
            ))
        );
    }
    s.push_str(&queue(&f.queue));
    s
}

pub fn revoked(r: &crate::daemon::wire::Revoked) -> String {
    let mut s = match &r.cert {
        Some(c) => format!("Revoked {}'s certificate ({c}).\n", r.host),
        None => format!("{} had no certificate pinned.\n", r.host),
    };
    if r.uncollected {
        let _ = writeln!(s, "Dropped the certificate waiting for it, uncollected.");
    }
    let _ = writeln!(
        s,
        "Its requests are refused; `replicant approve {}` issues a new certificate. Its \
         enrollment and subscriptions are unchanged.",
        r.host
    );
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
#[path = "render_fleet_tests.rs"]
mod tests;
