//! `replicant approve`: the interactive flow on home. PLAN.md, "Enrolling a
//! new machine". It forces the one decision that matters — whether this box
//! gets a cap — and echoes the normalized cap before writing it, so what is
//! approved is what was seen, not what was typed.
//!
//! I/O is passed in so the flow is tested with scripted input.

use crate::daemon::wire::{LocalReply, LocalRequest};
use crate::timefmt;
use crate::{Error, Result};
use std::io::{BufRead, Write};

/// Read one line; `None` at end of input.
fn ask(input: &mut dyn BufRead, out: &mut dyn Write, prompt: &str) -> Result<Option<String>> {
    write!(out, "{prompt}")?;
    out.flush()?;
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        writeln!(out)?;
        return Ok(None);
    }
    Ok(Some(line.trim().to_string()))
}

pub fn run(
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    only: Option<&str>,
    call: &dyn Fn(LocalRequest) -> Result<LocalReply>,
) -> Result<()> {
    let LocalReply::Pending { hosts } = call(LocalRequest::Pending)? else {
        return Err(Error::Daemon("unexpected reply to pending".into()));
    };
    if let Some(h) = only.filter(|h| !hosts.iter().any(|p| p.host == *h)) {
        return reissue(input, out, h, call);
    }
    if hosts.is_empty() {
        writeln!(out, "Nothing pending.")?;
        return Ok(());
    }
    let now = crate::now_secs();
    writeln!(out, "Pending:")?;
    for p in &hosts {
        writeln!(
            out,
            "  {:<10} {}   wants: {}   ({} ago)",
            p.host,
            p.home_dir.display(),
            p.paths.join(", "),
            timefmt::ago(now.saturating_sub(p.requested_at))
        )?;
    }
    for p in hosts.iter().filter(|p| only.is_none_or(|h| h == p.host)) {
        writeln!(out)?;
        let yes = ask(input, out, &format!("Approve {}? [y/N] ", p.host))?;
        if !matches!(yes.as_deref(), Some("y" | "Y" | "yes")) {
            writeln!(out, "Skipped {}.", p.host)?;
            continue;
        }
        let (raw, preview) = loop {
            let Some(raw) = ask(
                input,
                out,
                "Allow list (relative to home dir, space-separated, empty = anything):\n> ",
            )?
            else {
                writeln!(out, "Not approved.")?;
                return Ok(());
            };
            match call(LocalRequest::PreviewAllow {
                host: p.host.clone(),
                raw: vec![raw.clone()],
            }) {
                Ok(LocalReply::AllowPreview(pv)) => break (raw, pv),
                Ok(other) => return Err(Error::Daemon(format!("unexpected reply: {other:?}"))),
                Err(e) => writeln!(out, "  {e}")?,
            }
        };
        match &preview.allow {
            Some(a) => writeln!(out, "  → {}", a.join(", "))?,
            None => writeln!(out, "  → (no cap: anything home serves)")?,
        }
        if !preview.drop.is_empty() {
            writeln!(
                out,
                "  drops from its request: {} (outside the cap)",
                preview.drop.join(", ")
            )?;
        }
        let confirm = ask(input, out, "Confirm? [Y/n] ")?;
        if confirm.is_none() || matches!(confirm.as_deref(), Some("n" | "N" | "no")) {
            writeln!(out, "Not approved.")?;
            continue;
        }
        let LocalReply::Approved(a) = call(LocalRequest::Approve {
            host: p.host.clone(),
            allow: vec![raw],
        })?
        else {
            return Err(Error::Daemon("unexpected reply to approve".into()));
        };
        writeln!(out, "Approved. Run `replicant sync` on {} to pull.", p.host)?;
        writeln!(
            out,
            "  Its certificate ({}) waits here; {} collects it on its next request.",
            a.cert, p.host
        )?;
    }
    Ok(())
}

/// `approve <host>` for a host that is not pending: issue an enrolled host a
/// new certificate (a lost `host.pem`, or a fleet upgraded from v0.1).
fn reissue(
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    host: &str,
    call: &dyn Fn(LocalRequest) -> Result<LocalReply>,
) -> Result<()> {
    let LocalReply::Fleet(f) = call(LocalRequest::Fleet)? else {
        return Err(Error::Daemon("unexpected reply to fleet".into()));
    };
    if !f.hosts.iter().any(|h| h.name == host) {
        return Err(Error::Refused(format!("{host} is not pending or enrolled")));
    }
    let yes = ask(
        input,
        out,
        &format!(
            "{host} is enrolled. Issue it a new certificate? Any certificate it has now stops \
             working. [y/N] "
        ),
    )?;
    if !matches!(yes.as_deref(), Some("y" | "Y" | "yes")) {
        writeln!(out, "Not reissued.")?;
        return Ok(());
    }
    let LocalReply::Reissued(r) = call(LocalRequest::Reissue { host: host.into() })? else {
        return Err(Error::Daemon("unexpected reply to reissue".into()));
    };
    writeln!(
        out,
        "Issued {host} a new certificate ({}); it collects it on its next request.",
        r.cert
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "approve_tests.rs"]
mod tests;
