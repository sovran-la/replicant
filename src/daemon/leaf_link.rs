//! A leaf's link to home, and which certificate it presents.
//!
//! A leaf starts with home's bootstrap bundle (`home.pem`), good only for
//! asking to be enrolled and for collecting its own certificate once
//! `approve` issues one. Until it holds `host.pem`, every link first asks
//! home for that certificate; once collected, `host.pem` is the leaf's
//! identity for everything and `home.pem` stays only as the CA it trusts.

use super::Leaf;
use crate::config::LeafConfig;
use crate::daemon::certs;
use crate::daemon::journal::Journal;
use crate::daemon::remote::Dialer;
use crate::daemon::tls::{self, Identity};
use crate::daemon::wire::*;
use crate::plan::HomeLink;
use crate::{Error, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Long enough to wait out the queue and hash home's side of a large tree.
const REQUEST_DEADLINE: Duration = Duration::from_secs(900);
/// Heartbeats and releases: short, so a hung call cannot outlast the lease
/// it is meant to keep, and the heartbeat thread gets to retry.
const LEASE_CALL_DEADLINE: Duration = Duration::from_secs(10);

/// The mTLS link to home, as this leaf.
pub(super) struct Remote<'a> {
    journal: &'a Journal,
    address: String,
    me: String,
    home: String,
    home_dir: PathBuf,
    dialer: Dialer,
}

impl HomeLink for Remote<'_> {
    fn call(&self, request: HomeRequest) -> Result<HomeReply> {
        let deadline = match request {
            HomeRequest::Heartbeat { .. } | HomeRequest::Release { .. } => LEASE_CALL_DEADLINE,
            _ => REQUEST_DEADLINE,
        };
        let call = HomeCall {
            version: WIRE_VERSION,
            host: self.me.clone(),
            home_dir: self.home_dir.clone(),
            request,
        };
        let answer: HomeAnswer = self.dialer.call(&self.address, &call, deadline)?;
        for line in &answer.journal {
            if let Err(e) = self.journal.append_raw(line) {
                eprintln!("replicant: could not keep a local copy of a journal line: {e}");
            }
        }
        answer.result.map_err(Error::Remote)
    }

    fn open_data(&self, _rel: &str) -> Result<crate::plan::DataLink<'_>> {
        let conn = self
            .dialer
            .open_data(&self.address, crate::native::session::HELLO_TIMEOUT)?;
        Ok(crate::plan::DataLink { conn, serve: None })
    }
}

/// Which certificate the link presents.
pub(super) enum Link<'a> {
    /// `host.pem`: this leaf, as home knows it.
    Host(Remote<'a>),
    /// The bootstrap, and what home said when asked for a certificate.
    Bootstrap(Remote<'a>, Collected),
}

impl<'a> Link<'a> {
    /// The link as this host, or why there is none yet.
    pub(super) fn host(self) -> Result<Remote<'a>> {
        match self {
            Link::Host(r) => Ok(r),
            Link::Bootstrap(r, Collected::Pending { home, .. }) => Err(Error::Refused(format!(
                "{} is pending approval on {home}; run `replicant approve` there",
                r.me
            ))),
            Link::Bootstrap(r, Collected::Unknown { home }) => Err(Error::Refused(format!(
                "{} is not enrolled on {home}; run `replicant subscribe {home} <paths...>`",
                r.me
            ))),
            Link::Bootstrap(_, Collected::Issued { .. }) => {
                unreachable!("Leaf::link keeps an issued bundle")
            }
        }
    }
}

impl Leaf {
    /// The link to home: as this host if it has its certificate, else over
    /// the bootstrap after collecting one if `approve` has issued it.
    pub(super) fn link(&self, cfg: &LeafConfig) -> Result<Link<'_>> {
        let own = self.dirs.host_identity();
        if own.exists() {
            return Ok(Link::Host(self.remote(cfg, &own)?));
        }
        let boot_path = cfg.credentials(&self.dirs);
        let boot = self.remote(cfg, &boot_path)?;
        match boot.call(HomeRequest::CollectCert)? {
            HomeReply::Collected(Collected::Issued { bundle }) => {
                self.keep_identity(&boot_path, &bundle, &boot.home)?;
                Ok(Link::Host(self.remote(cfg, &own)?))
            }
            HomeReply::Collected(state) => Ok(Link::Bootstrap(boot, state)),
            other => Err(Error::Remote(format!("unexpected reply: {other:?}"))),
        }
    }

    /// Collect a reissued certificate over the bootstrap, replacing
    /// `host.pem` only if one arrives. Whether one did.
    pub(super) fn recollect(&self, cfg: &LeafConfig) -> Result<bool> {
        let boot_path = cfg.credentials(&self.dirs);
        let boot = self.remote(cfg, &boot_path)?;
        match boot.call(HomeRequest::CollectCert)? {
            HomeReply::Collected(Collected::Issued { bundle }) => {
                self.keep_identity(&boot_path, &bundle, &boot.home)?;
                Ok(true)
            }
            HomeReply::Collected(_) => Ok(false),
            other => Err(Error::Remote(format!("unexpected reply: {other:?}"))),
        }
    }

    fn remote(&self, cfg: &LeafConfig, identity: &Path) -> Result<Remote<'_>> {
        let identity = Identity::load(identity)?;
        Ok(Remote {
            journal: &self.journal,
            address: cfg.home.address.clone(),
            me: cfg.name()?,
            home: cfg.home.name.clone(),
            home_dir: cfg.home_dir()?,
            dialer: Dialer::new(&identity, CONNECT_TIMEOUT)?,
        })
    }

    /// Save a collected bundle as this leaf's identity, if it is one from
    /// the CA the bootstrap trusts. Home has already deleted its copy, so a
    /// failure here is recovered by reissuing on home.
    fn keep_identity(&self, boot: &Path, bundle: &str, home: &str) -> Result<()> {
        let own = self.dirs.host_identity();
        let lost = |why: String| {
            Error::Tls(format!(
                "collected this machine's certificate from {home} but could not keep it ({why}); \
                 run `replicant approve <this host>` on {home} to issue another"
            ))
        };
        let id = tls::parse(bundle, &own).map_err(|e| lost(e.to_string()))?;
        if id.ca != Identity::load(boot)?.ca {
            return Err(lost(format!(
                "it is not signed by the CA in {}",
                boot.display()
            )));
        }
        certs::write_secret_atomic(&own, bundle).map_err(|e| lost(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Dirs;

    fn ca(dir: &Path) -> certs::Paths {
        let p = certs::Paths::of(&Dirs::at(dir.to_path_buf()));
        certs::ensure(&p).unwrap();
        p
    }

    #[test]
    fn a_collected_bundle_is_kept_only_if_the_bootstrap_ca_signed_it() {
        let (home, stranger, leaf) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let (ours, theirs) = (ca(home.path()), ca(stranger.path()));
        let l = Leaf::open(Dirs::at(leaf.path().to_path_buf()));
        let own = l.dirs.host_identity();

        let foreign = certs::issue(&theirs, "leaf").unwrap().bundle;
        let err = l
            .keep_identity(&ours.bootstrap, &foreign, "home")
            .unwrap_err()
            .to_string();
        assert!(err.contains("not signed by the CA"), "{err}");
        assert!(
            err.contains("replicant approve"),
            "says how to recover: {err}"
        );
        assert!(!own.exists());
        let err = l
            .keep_identity(&ours.bootstrap, "garbage", "home")
            .unwrap_err()
            .to_string();
        assert!(err.contains("could not keep it"), "{err}");
        assert!(!own.exists());

        let good = certs::issue(&ours, "leaf").unwrap();
        l.keep_identity(&ours.bootstrap, &good.bundle, "home")
            .unwrap();
        assert_eq!(
            Identity::load(&own).unwrap().fingerprint(),
            good.fingerprint
        );
    }
}
