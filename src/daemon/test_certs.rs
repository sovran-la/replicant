//! Certificates and raw mTLS callers for the daemon tests.

use super::Daemon;
use super::certs;
use super::remote::Dialer;
use super::tls::Identity;
use super::wire::{HomeAnswer, HomeCall, HomeReply, HomeRequest, WIRE_VERSION};
use crate::testutil::Bed;
use crate::{Error, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A client identity for `host`, minted from home's CA, saved under the
/// bed's tempdir. Returns its path.
pub fn leaf_identity(bed: &Bed, host: &str) -> PathBuf {
    let p = certs::Paths::of(&bed.home_dirs);
    certs::ensure(&p).unwrap();
    let issued = certs::issue(&p, host).unwrap();
    let path = bed.tmp.path().join(format!("{host}-identity.pem"));
    certs::write_secret(&path, &issued.bundle).unwrap();
    path
}

/// A caller that speaks [`HomeCall`] to a running home directly, with any
/// identity and any claimed host name.
pub struct RawLeaf {
    dialer: Dialer,
    addr: String,
    pub host: String,
    pub hd: PathBuf,
}

pub fn dial(home: &Daemon, identity: &Path, host: &str, hd: &Path) -> RawLeaf {
    let id = Identity::load(identity).unwrap();
    RawLeaf {
        dialer: Dialer::new(&id, Duration::from_secs(5)).unwrap(),
        addr: home.tcp_addr().unwrap().to_string(),
        host: host.into(),
        hd: hd.to_path_buf(),
    }
}

impl RawLeaf {
    pub fn call(&self, request: HomeRequest) -> Result<HomeReply> {
        let call = HomeCall {
            version: WIRE_VERSION,
            host: self.host.clone(),
            home_dir: self.hd.clone(),
            request,
        };
        let a: HomeAnswer = self
            .dialer
            .call(&self.addr, &call, Duration::from_secs(5))?;
        a.result.map_err(Error::Remote)
    }

    /// The same identity, claiming another name.
    pub fn as_host(&self, host: &str) -> RawLeaf {
        RawLeaf {
            dialer: self.dialer.clone(),
            addr: self.addr.clone(),
            host: host.into(),
            hd: self.hd.clone(),
        }
    }

    pub fn refused(&self, request: HomeRequest) -> String {
        let what = format!("{request:?}");
        match self.call(request) {
            Err(e) => e.to_string(),
            Ok(r) => panic!("{what} as {} was not refused: {r:?}", self.host),
        }
    }
}
