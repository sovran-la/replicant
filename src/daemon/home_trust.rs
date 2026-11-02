//! Who is asking: home's half of the trust model (PLAN.md, "Control plane").
//!
//! The handshake proved the peer holds *a* certificate home's CA signed.
//! Which one decides what it may do:
//!
//! - **the bootstrap** (`credentials.pem`, every new leaf's `home.pem`): may
//!   ask to be enrolled (`Subscribe` from a host with no `[hosts.*]` entry)
//!   and nothing else. The name it gives is self-asserted, so the bootstrap
//!   never acts for an enrolled host.

use super::Home;
use crate::daemon::wire::HomeRequest;
use crate::{Error, Result};

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
     subscribe` from a machine home has not enrolled";

impl Home {
    /// Decide whether `caller`, claiming to be `claimed`, may make `request`.
    pub(super) fn admit(
        &self,
        caller: &Caller,
        claimed: &str,
        request: &HomeRequest,
    ) -> Result<Admitted> {
        let Some(fp) = caller.fingerprint() else {
            return Ok(Admitted {
                host: claimed.into(),
                bootstrap: false,
            });
        };
        if self.bootstrap.as_deref() == Some(fp) {
            return match request {
                HomeRequest::Subscribe { .. } => Ok(Admitted {
                    host: claimed.into(),
                    bootstrap: true,
                }),
                _ => Err(Error::Refused(format!(
                    "{BOOTSTRAP_ONLY}; {claimed} must use its own certificate"
                ))),
            };
        }
        Ok(Admitted {
            host: claimed.into(),
            bootstrap: false,
        })
    }

    /// `Subscribe` over the bootstrap, under the fleet lock it will write
    /// with: refused for an enrolled host, whose own certificate must ask.
    pub(super) fn bootstrap_may_subscribe(
        &self,
        f: &crate::config::Fleet,
        host: &str,
    ) -> Result<()> {
        if f.hosts.contains_key(host) {
            return Err(Error::Refused(format!(
                "{host} is enrolled on {}; {BOOTSTRAP_ONLY}, and {host} must use its own \
                 certificate",
                f.name
            )));
        }
        Ok(())
    }
}
