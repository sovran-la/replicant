//! `approve` on home: preview a typed cap against a pending host, and
//! enroll it. PLAN.md, "Enrolling a new machine".

use super::{normalizer, outside_cap};
use crate::config::{Fleet, HostEntry, Subscription, edit};
use crate::daemon::wire::{AllowPreview, Approved};
use crate::paths;
use crate::{Error, Result};
use std::path::Path;

/// Normalize a typed allow list against a pending host, and show what it keeps.
pub fn preview_allow(fleet: &Fleet, host: &str, raw: &[String]) -> Result<AllowPreview> {
    let p = fleet
        .pending
        .get(host)
        .ok_or_else(|| Error::Refused(format!("{host} is not pending on {}", fleet.name)))?;
    let roots = fleet.root_rels();
    let words: Vec<String> = raw
        .iter()
        .flat_map(|r| r.split_whitespace())
        .map(String::from)
        .collect();
    let allow = if words.is_empty() {
        None
    } else {
        Some(paths::collapse(&normalizer(fleet, &p.home_dir, &roots).normalize(&words)?).0)
    };
    let drop = outside_cap(&p.paths, allow.as_deref());
    let keep = p
        .paths
        .iter()
        .filter(|x| !drop.contains(x))
        .cloned()
        .collect();
    Ok(AllowPreview {
        host: host.into(),
        allow,
        keep,
        drop,
    })
}

/// Approve a pending host with a cap (empty = none), pinning `cert` (the
/// fingerprint of the client certificate home minted for it) in the same
/// write. Never starts a sync.
pub fn approve(
    fleet: &Fleet,
    file: &Path,
    host: &str,
    raw_allow: &[String],
    cert: &str,
) -> Result<Approved> {
    let preview = preview_allow(fleet, host, raw_allow)?;
    let p = &fleet.pending[host];
    edit::approve(
        file,
        host,
        &HostEntry {
            home_dir: p.home_dir.clone(),
            allow: preview.allow.clone(),
            cert: Some(cert.into()),
        },
        &Subscription {
            paths: preview.keep.clone(),
            ..Default::default()
        },
    )?;
    Ok(Approved {
        host: host.into(),
        allow: preview.allow,
        subscribed: preview.keep,
        dropped: preview.drop,
        cert: cert.into(),
    })
}
