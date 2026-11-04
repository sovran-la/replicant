//! Certificates and raw mTLS callers for the daemon tests.

use super::Daemon;
use super::certs;
use super::remote::Dialer;
use super::tls::Identity;
use super::wire::{HomeAnswer, HomeCall, HomeReply, HomeRequest, WIRE_VERSION};
use crate::config::{HomeEntry, LeafConfig};
use crate::testutil::Bed;
use crate::{Error, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A client identity for `host`, minted from home's CA and saved under the
/// bed's tempdir, as approve would: pinned in fleet.toml if `host` is
/// enrolled there, unpinned (unknown to home) if not. Returns its path.
pub fn leaf_identity(bed: &Bed, host: &str) -> PathBuf {
    let p = certs::Paths::of(&bed.home_dirs);
    certs::ensure(&p).unwrap();
    let issued = certs::issue(&p, host).unwrap();
    let fleet = bed.home_dirs.fleet();
    if crate::config::Fleet::load(&fleet)
        .unwrap()
        .hosts
        .contains_key(host)
    {
        crate::config::edit::set_cert(&fleet, host, Some(&issued.fingerprint)).unwrap();
    }
    let path = bed.tmp.path().join(format!("{host}-identity.pem"));
    certs::write_secret(&path, &issued.bundle).unwrap();
    path
}

/// Give the bed's leaf home's bootstrap bundle as `home.pem`, and, if
/// fleet.toml enrolls it, a certificate of its own as `host.pem`.
pub fn equip_leaf(bed: &Bed) {
    let p = certs::Paths::of(&bed.home_dirs);
    certs::ensure(&p).unwrap();
    std::fs::copy(&p.bootstrap, bed.leaf_dirs.home_bundle()).unwrap();
    let f = crate::config::Fleet::load(&bed.home_dirs.fleet()).unwrap();
    if f.hosts.contains_key("leaf") {
        std::fs::copy(leaf_identity(bed, "leaf"), bed.leaf_dirs.host_identity()).unwrap();
    }
}

pub fn leaf_toml(bed: &Bed, address: &str) {
    LeafConfig {
        name: Some("leaf".into()),
        home_dir: Some(bed.leaf_hd.clone()),
        home: HomeEntry {
            name: "home".into(),
            address: crate::config::Addresses::one(address),
            credentials: None,
            rsync_host: None,
            rsync_timeout_secs: None,
            streams: None,
        },
        as_local: Default::default(),
    }
    .save(&bed.leaf_dirs.leaf())
    .unwrap();
}

/// Home and leaf daemons: the leaf holds home's bootstrap bundle, and its
/// own certificate if fleet.toml already enrolls it.
pub fn pair(bed: &Bed) -> (Daemon, Daemon) {
    let home = Daemon::start(bed.home_dirs.clone()).unwrap();
    let addr = home.tcp_addr().unwrap().to_string();
    equip_leaf(bed);
    leaf_toml(bed, &addr);
    let leaf = Daemon::start(bed.leaf_dirs.clone()).unwrap();
    (home, leaf)
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

    /// A data connection, said hello on: home's answer is the result.
    pub fn data(&self, hello: &crate::native::session::Hello) -> Result<crate::native::conn::Conn> {
        let mut c = self.dialer.open_data(&self.addr, Duration::from_secs(5))?;
        crate::native::session::open(&mut c, hello)?;
        Ok(c)
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

/// One of each of apply's requests to home, for the trust tests: each must
/// be refused as the others are.
pub fn apply_requests() -> Vec<HomeRequest> {
    vec![
        HomeRequest::BeginPush {
            lease: 1,
            rel: "work".into(),
            source_state: crate::state::UnitState::Empty,
            is_file: false,
        },
        HomeRequest::Rehash {
            lease: 1,
            rel: "work".into(),
            kind: crate::units::UnitKind::Loose,
            union: vec![],
        },
        HomeRequest::Agree {
            lease: 1,
            rel: "work".into(),
            baseline: None,
            rollback_accepted: false,
        },
        HomeRequest::Settle {
            lease: 1,
            rel: "work".into(),
            direction: None,
            detail: String::new(),
            changes: vec![],
        },
        HomeRequest::Interrupted {
            rel: "work".into(),
            direction: crate::marker::Direction::Push,
            why: String::new(),
            changes: vec![],
        },
        HomeRequest::Note {
            event: "adopt".into(),
            rel: "work".into(),
            detail: String::new(),
        },
    ]
}
