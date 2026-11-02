//! Enrollment on home: `approve` enrolls a pending host and mints its client
//! certificate, `approve` on an enrolled host reissues one. PLAN.md,
//! "Enrolling a new machine".
//!
//! A minted certificate is pinned in fleet.toml and its bundle waits in
//! `state/issued/<host>.pem` until the host collects it. Both happen under
//! the fleet lock, so a collect never sees a pin without its bundle.

use super::Home;
use crate::config::{edit, short_hostname};
use crate::daemon::certs;
use crate::daemon::journal::Line;
use crate::daemon::wire::{Approved, Reissued};
use crate::{Error, Result, subscribe};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

impl Home {
    /// Approve a pending host with a cap (empty = none), and mint and pin its
    /// certificate in the same fleet.toml write.
    pub fn approve(&self, host: &str, raw: &[String]) -> Result<Approved> {
        let mut f = self.fleet()?;
        let issued = self.mint(host)?;
        let out = subscribe::approve(&f, &self.dirs.fleet(), host, raw, &issued.fingerprint)?;
        self.reload(&mut f)?;
        self.leave_for_collection(host, &issued.bundle)?;
        let allow = out
            .allow
            .as_ref()
            .map(|a| a.join(" "))
            .unwrap_or_else(|| "(none)".into());
        let detail = format!("allow:{allow} cert:{}", issued.fingerprint);
        self.journal(&mut Vec::new(), Line::new("approve", host, "", detail));
        Ok(out)
    }

    /// A new certificate for an enrolled host, pinned in place of its old
    /// one (which stops working at once), waiting for it to collect.
    pub fn reissue(&self, host: &str) -> Result<Reissued> {
        let mut f = self.fleet()?;
        let Some(entry) = f.hosts.get(host) else {
            return Err(Error::Refused(format!(
                "{host} is not enrolled on {}; a pending host is approved, not reissued",
                f.name
            )));
        };
        let replaced = entry.cert.clone();
        let issued = self.mint(host)?;
        edit::set_cert(&self.dirs.fleet(), host, Some(&issued.fingerprint))?;
        self.reload(&mut f)?;
        self.leave_for_collection(host, &issued.bundle)?;
        let detail = format!(
            "cert:{} replaces:{}",
            issued.fingerprint,
            replaced.as_deref().unwrap_or("(none)")
        );
        self.journal(&mut Vec::new(), Line::new("reissue", host, "", detail));
        Ok(Reissued {
            host: host.into(),
            cert: issued.fingerprint,
            replaced,
        })
    }

    fn mint(&self, host: &str) -> Result<certs::Issued> {
        self.issued_path(host)?;
        certs::issue(&certs::Paths::of(&self.dirs), host)
    }

    /// Where `host`'s issued bundle waits. The name becomes a file name, so
    /// it must be a fleet name and nothing else (fleet.toml is hand-edited).
    pub(super) fn issued_path(&self, host: &str) -> Result<PathBuf> {
        if short_hostname(host).as_deref() != Some(host) {
            return Err(Error::Refused(format!("{host:?} is not a valid host name")));
        }
        Ok(self.dirs.issued().join(format!("{host}.pem")))
    }

    /// Save the bundle for `host` to collect. Runs after the pin is written:
    /// if this fails the pin names a certificate nobody holds, which refuses
    /// everything and is fixed by reissuing.
    fn leave_for_collection(&self, host: &str, bundle: &str) -> Result<()> {
        let path = self.issued_path(host)?;
        let save = || -> Result<()> {
            std::fs::create_dir_all(self.dirs.issued())?;
            std::fs::set_permissions(self.dirs.issued(), std::fs::Permissions::from_mode(0o700))?;
            certs::write_secret_atomic(&path, bundle)
        };
        save().map_err(|e| {
            Error::Daemon(format!(
                "{host}'s certificate is pinned but could not be saved for collection ({e}); \
                 run `replicant approve {host}` again to reissue it"
            ))
        })
    }
}
