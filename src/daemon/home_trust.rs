//! Who is asking: home's half of the trust model (PLAN.md, "Control plane").
//!
//! The handshake proved the peer holds *a* certificate home's CA signed.
//! Which one decides what it may do:
//!
//! - **the bootstrap** (`credentials.pem`, every new leaf's `home.pem`): may
//!   ask to be enrolled (`Subscribe` from a host with no `[hosts.*]` entry)
//!   and collect the certificate `approve` issued (`CollectCert`), nothing
//!   else. The name it gives is self-asserted, so the bootstrap never acts
//!   for an enrolled host.
//! - **a pinned certificate** (`[hosts.x] cert` in fleet.toml): *is* host
//!   `x`. The request's own `host` field is not identity; one naming another
//!   host is refused and journaled.
//! - **anything else** (unpinned: revoked, replaced by a reissue, or minted
//!   by a home that no longer pins it): refused and journaled.
//!
//! Pins are read from fleet.toml on every request, so a hand edit (a pin
//! removed by hand) holds on the next one (F11). Heartbeat and Release must
//! survive a broken fleet.toml (review P5), so they alone fall back to the
//! pins last read from a good one: identity, never policy.

use super::Home;
use crate::config::Fleet;
use crate::daemon::journal::Line;
use crate::daemon::wire::HomeRequest;
use crate::{Error, Result};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Who the handshake says is on the line.
pub enum Caller {
    /// Home's own tests, calling in-process as the named host.
    #[cfg(test)]
    Trusted,
    /// A TLS peer, by its certificate's fingerprint.
    Cert(String),
}

impl Caller {
    /// The peer's fingerprint; `None` for an in-process test caller.
    fn fingerprint(&self) -> Option<&str> {
        match self {
            #[cfg(test)]
            Caller::Trusted => None,
            Caller::Cert(fp) => Some(fp),
        }
    }
}

/// What a request may act as, once admitted.
pub(super) struct Admitted {
    pub host: String,
    /// Came over the bootstrap bundle: `Subscribe` must re-check, under the
    /// fleet lock it writes with, that `host` is not enrolled.
    pub bootstrap: bool,
}

/// The refusal for everything the bootstrap may not do.
pub const BOOTSTRAP_ONLY: &str = "this is home's bootstrap bundle, good only for `replicant \
     subscribe` from a machine home has not enrolled and for collecting the certificate \
     `approve` issues it";

/// The refusal for a certificate home does not pin. A leaf seeing it asks
/// for a reissued certificate over the bootstrap.
pub const UNKNOWN_CERT: &str = "home does not know this certificate";

/// Fingerprint → host, as of the last fleet.toml that parsed.
#[derive(Default)]
pub(super) struct Pins(Mutex<BTreeMap<String, String>>);

impl Pins {
    fn remember(&self, f: &Fleet) {
        let pins: BTreeMap<String, String> = f
            .hosts
            .iter()
            .filter_map(|(h, e)| e.cert.clone().map(|c| (c, h.clone())))
            .collect();
        let mut held = self.0.lock().unwrap_or_else(|e| e.into_inner());
        *held = pins;
    }

    fn get(&self, fp: &str) -> Option<String> {
        let held = self.0.lock().unwrap_or_else(|e| e.into_inner());
        held.get(fp).cloned()
    }
}

impl Home {
    /// Decide whether `caller`, claiming to be `claimed`, may make `request`,
    /// and as which host.
    pub(super) fn admit(
        &self,
        caller: &Caller,
        claimed: &str,
        request: &HomeRequest,
        j: &mut Vec<String>,
    ) -> Result<Admitted> {
        let Some(fp) = caller.fingerprint() else {
            return Ok(Admitted {
                host: claimed.into(),
                bootstrap: false,
            });
        };
        if self.bootstrap.as_deref() == Some(fp) {
            return match request {
                HomeRequest::Subscribe { .. } | HomeRequest::CollectCert => Ok(Admitted {
                    host: claimed.into(),
                    bootstrap: true,
                }),
                _ => Err(Error::Refused(format!(
                    "{BOOTSTRAP_ONLY}; {claimed} must use its own certificate"
                ))),
            };
        }
        let lease_op = matches!(
            request,
            HomeRequest::Heartbeat { .. } | HomeRequest::Release { .. }
        );
        let Some(host) = self.pinned(fp, lease_op)? else {
            let detail = format!("unknown certificate {fp}; the request named {claimed}");
            self.journal(j, Line::new("refused", claimed, "", detail));
            return Err(Error::Refused(format!(
                "{UNKNOWN_CERT} ({fp}): no [hosts.*] entry in fleet.toml pins it (revoked, or \
                 replaced by `replicant approve {claimed}`), and it is not the bootstrap"
            )));
        };
        if host != claimed {
            let detail = format!("certificate is {host}'s; the request named {claimed}");
            self.journal(j, Line::new("refused", &host, "", detail));
            return Err(Error::Refused(format!(
                "this certificate is {host}'s, and the request names {claimed}; a host acts \
                 only as itself"
            )));
        }
        Ok(Admitted {
            host,
            bootstrap: false,
        })
    }

    /// The host `fp` is pinned for, read from fleet.toml now. A lease
    /// operation alone may fall back to the last good pins.
    fn pinned(&self, fp: &str, lease_op: bool) -> Result<Option<String>> {
        match self.fleet() {
            Ok(f) => {
                self.pins.remember(&f);
                Ok(f.host_for_cert(fp).map(String::from))
            }
            Err(_) if lease_op => Ok(self.pins.get(fp)),
            Err(e) => Err(e),
        }
    }

    /// `Subscribe` over the bootstrap, under the fleet lock it will write
    /// with: refused for an enrolled host, whose own certificate must ask.
    pub(super) fn bootstrap_may_subscribe(&self, f: &Fleet, host: &str) -> Result<()> {
        if f.hosts.contains_key(host) {
            return Err(Error::Refused(format!(
                "{host} is enrolled on {}; {BOOTSTRAP_ONLY}, and {host} must use its own \
                 certificate",
                f.name
            )));
        }
        Ok(())
    }

    /// A lease is heartbeated or released only by the host that holds it.
    pub(super) fn holds(&self, lease: u64, host: &str) -> Result<()> {
        match self.queue.view().into_iter().find(|e| e.id == lease) {
            Some(e) if e.host != host => Err(Error::Refused(format!(
                "lease {lease} is {}'s, not {host}'s",
                e.host
            ))),
            _ => Ok(()),
        }
    }
}
