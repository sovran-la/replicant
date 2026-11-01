//! What home serves a host: its in-bounds subscriptions (guardrail 5 on
//! home's side), the subscriptions view, and home's half of the state
//! exchange.

use super::Home;
use crate::config::Fleet;
use crate::daemon::wire::{Enrollment, HomeSnapshot, SubscriptionsView};
use crate::guard::{self, Refusal};
use crate::marker::{self, Marker};
use crate::units::{self, Discovered, SubSpec};
use crate::{Error, Result, paths};

impl Home {
    /// What `host` may be served: in-bounds specs, and refusals for the rest.
    pub(super) fn served(&self, f: &Fleet, host: &str) -> Result<(Vec<SubSpec>, Vec<Refusal>)> {
        let Some(entry) = f.hosts.get(host) else {
            return Err(Error::Refused(if f.pending.contains_key(host) {
                format!(
                    "{host} is pending approval; run `replicant approve` on {}",
                    f.name
                )
            } else {
                format!(
                    "{host} is not enrolled on {}; run `replicant subscribe {} <paths>`",
                    f.name, f.name
                )
            }));
        };
        let sub = f.subscription(host);
        let (mut specs, mut refused) = (Vec::new(), Vec::new());
        for p in &sub.paths {
            let root = f.root_for(p);
            let cap_refusal = guard::bounds(p, &sub.paths, &[], entry.allow.as_deref());
            match (root, cap_refusal) {
                (Some(r), None) => specs.push(SubSpec {
                    rel: p.clone(),
                    exclude: r.exclude.clone(),
                    except: sub
                        .except
                        .iter()
                        .filter(|e| paths::is_within(e, p))
                        .cloned()
                        .collect(),
                }),
                (None, _) => refused.push(Refusal {
                    guard: guard::Guard::SubscriptionBounds,
                    unit: p.clone(),
                    detail: vec!["no longer under any root home serves".into()],
                }),
                (_, Some(r)) => refused.push(r),
            }
        }
        Ok((specs, refused))
    }

    pub(super) fn view(&self, host: &str) -> Result<SubscriptionsView> {
        let f = self.fleet()?;
        let base = SubscriptionsView {
            home: f.name.clone(),
            enrollment: Enrollment::Unknown,
            specs: vec![],
            cap: None,
            refused: vec![],
            pending_paths: vec![],
            lease_secs: f.lease.as_secs(),
        };
        Ok(match self.served(&f, host) {
            Ok((specs, refused)) => SubscriptionsView {
                enrollment: Enrollment::Enrolled,
                specs,
                refused,
                cap: f.hosts[host].allow.clone(),
                ..base
            },
            Err(_) => match f.pending.get(host) {
                Some(p) => SubscriptionsView {
                    enrollment: Enrollment::Pending,
                    pending_paths: p.paths.clone(),
                    ..base
                },
                None => base,
            },
        })
    }

    /// Home's side of every unit `host` is subscribed to.
    pub(super) fn snapshot(
        &self,
        host: &str,
        leaf_repos: &[String],
        case_check: bool,
    ) -> Result<HomeSnapshot> {
        let (specs, home_dir) = {
            let f = self.fleet()?;
            let specs = self.served(&f, host)?.0;
            // A missing root is home's own misconfiguration or an unmounted
            // volume, never "empty" (review F4); `served` kept only specs
            // under a root.
            let mut missing: Vec<String> = specs
                .iter()
                .filter_map(|s| f.root_for(&s.rel))
                .filter(|r| !r.path.exists())
                .map(|r| {
                    format!(
                        "root {} ({}) does not exist on {}",
                        r.name,
                        r.path.display(),
                        f.name
                    )
                })
                .collect();
            missing.sort();
            missing.dedup();
            if !missing.is_empty() {
                return Err(Error::Missing(missing.join("; ")));
            }
            (specs, f.home_dir.clone())
        };
        let hd = home_dir.clone();
        let t0 = std::time::Instant::now();
        let found = Discovered::run(&specs, &move |rel| hd.join(rel))?;
        let discover_ms = t0.elapsed().as_millis();
        let mut union = found.repos();
        // Guardrail 5 on home: a leaf's repo list cannot steer home outside
        // the host's bounds. Anything malformed or out of bounds is ignored.
        for r in leaf_repos {
            let ok = paths::check_rel(r).is_ok()
                && specs.iter().any(|s| {
                    paths::is_within(r, &s.rel) && !s.except.iter().any(|e| paths::is_within(r, e))
                });
            if ok && !union.contains(r) {
                union.push(r.clone());
            }
        }
        union.sort();
        let t1 = std::time::Instant::now();
        let (units, slowest) = units::snapshot_timed(&found, &union, case_check)?;
        // Where home's time goes, for whoever is staring at a slow `status`.
        eprintln!(
            "snapshot for {host}: discover {discover_ms}ms, units {}ms ({} units on {} workers, case_check {case_check}); slowest: {}",
            t1.elapsed().as_millis(),
            units.len(),
            crate::state::workers(),
            slowest
                .iter()
                .map(|(rel, ms)| format!("{rel} {ms}ms"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let markers: Vec<Marker> = marker::store(&self.dirs.state())
            .all::<Marker>()?
            .into_iter()
            .filter(|m| specs.iter().any(|s| m.covers(&s.rel)))
            .collect();
        let absent = found.absent().into_iter().map(|(rel, _)| rel).collect();
        Ok(HomeSnapshot {
            absent,
            home_dir,
            specs,
            units,
            repos: found.repos(),
            worktrees: found.worktrees(),
            markers,
        })
    }
}
