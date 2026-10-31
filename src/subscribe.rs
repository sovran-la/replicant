//! Home's side of `subscribe`, `unsubscribe` and `approve`: pending hosts,
//! cap enforcement, and the writes to fleet.toml. PLAN.md, "Subscriptions"
//! and "Enrolling a new machine".
//!
//! Each function takes the fleet as loaded and the path to write; the caller
//! (home's daemon, holding its fleet lock) reloads afterwards.

use crate::config::{Fleet, HostEntry, Pending, Subscription, edit};
use crate::daemon::wire::{AllowPreview, Approved, SubscribeOutcome, UnsubscribeOutcome};
use crate::paths::{self, Normalizer};
use crate::{Error, Result};
use std::path::Path;

fn normalizer<'a>(fleet: &'a Fleet, leaf_home: &'a Path, roots: &'a [String]) -> Normalizer<'a> {
    Normalizer {
        leaf_home,
        home_home: &fleet.home_dir,
        roots,
    }
}

/// Paths outside a cap, if there is one.
pub fn outside_cap(paths: &[String], cap: Option<&[String]>) -> Vec<String> {
    match cap {
        None => vec![],
        Some(cap) => paths
            .iter()
            .filter(|p| !cap.iter().any(|a| paths::is_within(p, a)))
            .cloned()
            .collect(),
    }
}

/// A subscribe request from `host`, whose `$HOME` is `leaf_home`.
///
/// An enrolled host's paths are checked against its cap and merged into its
/// subscription. An unknown host is recorded as pending and served nothing.
pub fn subscribe(
    fleet: &Fleet,
    file: &Path,
    host: &str,
    leaf_home: &Path,
    raw: &[String],
) -> Result<SubscribeOutcome> {
    if raw.is_empty() {
        return Err(Error::Path("subscribe needs at least one path".into()));
    }
    let roots = fleet.root_rels();
    let enrolled = fleet.hosts.get(host);
    // An enrolled host's home dir is what approve recorded, not what it says now.
    let hd = enrolled.map(|e| e.home_dir.as_path()).unwrap_or(leaf_home);
    let requested = normalizer(fleet, hd, &roots).normalize(raw)?;

    let Some(entry) = enrolled else {
        let mut all = fleet
            .pending
            .get(host)
            .map(|p| p.paths.clone())
            .unwrap_or_default();
        all.extend(requested.iter().cloned());
        let (paths, absorbed) = paths::collapse(&all);
        let p = Pending {
            home_dir: leaf_home.to_path_buf(),
            paths: paths.clone(),
            requested_at: crate::now_secs(),
        };
        edit::set_pending(file, host, &p)?;
        return Ok(SubscribeOutcome {
            enrolled: false,
            home: fleet.name.clone(),
            requested,
            paths,
            absorbed,
        });
    };

    let over = outside_cap(&requested, entry.allow.as_deref());
    if !over.is_empty() {
        return Err(Error::Refused(format!(
            "{host} is capped to {}; refusing {}. The cap is set on {} with `replicant approve` \
             and can only be widened there",
            entry.allow.as_deref().unwrap_or_default().join(", "),
            over.join(", "),
            fleet.name
        )));
    }
    let current = fleet.subscription(host);
    let mut all = current.paths.clone();
    all.extend(requested.iter().cloned());
    let (paths, absorbed) = paths::collapse(&all);
    // Re-subscribing a carved-out path (or a parent of one) restores it.
    let except: Vec<String> = current
        .except
        .into_iter()
        .filter(|e| !requested.iter().any(|r| paths::is_within(e, r)))
        .collect();
    edit::set_subscription(
        file,
        host,
        &Subscription {
            paths: paths.clone(),
            except,
        },
    )?;
    Ok(SubscribeOutcome {
        enrolled: true,
        home: fleet.name.clone(),
        requested,
        paths,
        absorbed,
    })
}

/// An unsubscribe request. A whole subscription is removed; a path inside a
/// broader one is carved out with `except`; a path above subscriptions
/// removes every one beneath it.
pub fn unsubscribe(
    fleet: &Fleet,
    file: &Path,
    host: &str,
    leaf_home: &Path,
    raw: &[String],
) -> Result<UnsubscribeOutcome> {
    let roots = fleet.root_rels();
    let hd = fleet
        .hosts
        .get(host)
        .map(|e| e.home_dir.as_path())
        .unwrap_or(leaf_home);
    let gone = normalizer(fleet, hd, &roots).normalize(raw)?;

    if !fleet.hosts.contains_key(host) {
        let Some(p) = fleet.pending.get(host) else {
            return Err(Error::Refused(format!(
                "{host} has no subscriptions on {}",
                fleet.name
            )));
        };
        let left: Vec<String> = p
            .paths
            .iter()
            .filter(|x| !gone.iter().any(|g| paths::is_within(x, g)))
            .cloned()
            .collect();
        let removed: Vec<String> = p
            .paths
            .iter()
            .filter(|x| !left.contains(x))
            .cloned()
            .collect();
        if left.is_empty() {
            edit::remove_pending(file, host)?;
        } else {
            edit::set_pending(
                file,
                host,
                &Pending {
                    paths: left.clone(),
                    ..p.clone()
                },
            )?;
        }
        return Ok(UnsubscribeOutcome {
            removed,
            carved: vec![],
            paths: left,
            except: vec![],
        });
    }

    let sub = fleet.subscription(host);
    let (mut paths_left, mut except) = (sub.paths.clone(), sub.except.clone());
    let (mut removed, mut carved, mut unknown) = (vec![], vec![], vec![]);
    for g in &gone {
        let beneath: Vec<String> = paths_left
            .iter()
            .filter(|p| paths::is_within(p, g))
            .cloned()
            .collect();
        if !beneath.is_empty() {
            paths_left.retain(|p| !beneath.contains(p));
            except.retain(|e| !paths::is_within(e, g));
            removed.extend(beneath);
        } else if paths_left.iter().any(|p| paths::is_within(g, p)) {
            if !except.iter().any(|e| paths::is_within(g, e)) {
                except.retain(|e| !paths::is_within(e, g));
                except.push(g.clone());
                carved.push(g.clone());
            }
        } else {
            unknown.push(g.clone());
        }
    }
    if !unknown.is_empty() {
        return Err(Error::Refused(format!(
            "{host} is not subscribed to {} (subscriptions: {})",
            unknown.join(", "),
            if sub.paths.is_empty() {
                "none".into()
            } else {
                sub.paths.join(", ")
            }
        )));
    }
    except.sort();
    edit::set_subscription(
        file,
        host,
        &Subscription {
            paths: paths_left.clone(),
            except: except.clone(),
        },
    )?;
    Ok(UnsubscribeOutcome {
        removed,
        carved,
        paths: paths_left,
        except,
    })
}

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

/// Approve a pending host with a cap (empty = none). Never starts a sync.
pub fn approve(fleet: &Fleet, file: &Path, host: &str, raw_allow: &[String]) -> Result<Approved> {
    let preview = preview_allow(fleet, host, raw_allow)?;
    let p = &fleet.pending[host];
    edit::set_host(
        file,
        host,
        &HostEntry {
            home_dir: p.home_dir.clone(),
            allow: preview.allow.clone(),
        },
    )?;
    edit::set_subscription(
        file,
        host,
        &Subscription {
            paths: preview.keep.clone(),
            except: vec![],
        },
    )?;
    edit::remove_pending(file, host)?;
    Ok(Approved {
        host: host.into(),
        allow: preview.allow,
        subscribed: preview.keep,
        dropped: preview.drop,
    })
}

#[cfg(test)]
#[path = "subscribe_tests.rs"]
mod tests;
