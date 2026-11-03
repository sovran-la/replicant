//! leaf.toml: where home is, and nothing else worth having opinions about.
//!
//! Written by `subscribe`. Subscriptions themselves live on home (home is the
//! record of truth); the only per-path thing a leaf keeps is `--as`, because a
//! local path override is meaningless anywhere but here.

use crate::paths;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// rsync_host value meaning "home's paths are on this machine's filesystem".
///
/// Only for a home daemon running on the same machine as the leaf (tests, and
/// the dogfood timing run); a real leaf rsyncs over ssh.
pub const LOCAL_RSYNC: &str = "local";

/// Default wall-clock bound on one rsync to or from home. A dry run of a whole
/// work tree takes seconds; past this, something is wedged (a partition, a
/// dead ssh) and is holding the head of home's queue.
pub const DEFAULT_RSYNC_TIMEOUT_SECS: u64 = 300;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomeEntry {
    /// Home's fleet name.
    pub name: String,
    /// Where home's daemon listens, `host:port`.
    pub address: String,
    /// Home's bundle; relative paths are beside leaf.toml.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials: Option<String>,
    /// The ssh destination rsync uses for home. Defaults to `name`;
    /// [`LOCAL_RSYNC`] means no ssh at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rsync_host: Option<String>,
    /// Kill an rsync that has not finished after this many seconds. Defaults
    /// to [`DEFAULT_RSYNC_TIMEOUT_SECS`]; at least 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rsync_timeout_secs: Option<u64>,
    /// Data connections per unit, at most home's `[daemon] streams`.
    /// Defaults to home's; at least 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streams: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeafConfig {
    /// This machine's fleet name. Defaults to the hostname.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// This machine's home dir as replicant maps paths. Defaults to `$HOME`;
    /// set for tests and same-machine experiments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_dir: Option<PathBuf>,
    pub home: HomeEntry,
    /// `--as`: normalized home path → absolute local path.
    #[serde(default, rename = "as", skip_serializing_if = "BTreeMap::is_empty")]
    pub as_local: BTreeMap<String, PathBuf>,
}

impl LeafConfig {
    pub fn load(path: &Path) -> Result<Option<Self>> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(Error::Config(format!(
                    "could not read {}: {e}",
                    path.display()
                )));
            }
        };
        let cfg: Self = toml::from_str(&text)
            .map_err(|e| Error::Config(format!("{} is not valid: {e}", path.display())))?;
        for (rel, local) in &cfg.as_local {
            paths::check_rel(rel)
                .map_err(|why| Error::Config(format!("[as] key {rel:?} {why}")))?;
            check_local(local)?;
        }
        Ok(Some(cfg))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string(self)
            .map_err(|e| Error::Config(format!("could not serialize leaf.toml: {e}")))?;
        super::write_atomic(path, &text)
    }

    /// This machine's fleet name.
    pub fn name(&self) -> Result<String> {
        match &self.name {
            Some(n) => Ok(n.clone()),
            None => super::hostname()
                .ok_or_else(|| Error::Config("no usable hostname; set `name` in leaf.toml".into())),
        }
    }

    /// The home dir paths are mapped through.
    pub fn home_dir(&self) -> Result<PathBuf> {
        match &self.home_dir {
            Some(h) if h.is_absolute() => Ok(h.clone()),
            Some(h) => Err(Error::Config(format!(
                "home_dir {} must be absolute",
                h.display()
            ))),
            None => super::user_home(),
        }
    }

    /// The ssh destination for rsync, or `None` for a same-machine home.
    pub fn rsync_host(&self) -> Option<String> {
        match self.home.rsync_host.as_deref() {
            Some(LOCAL_RSYNC) => None,
            Some(h) => Some(h.to_string()),
            None => Some(self.home.name.clone()),
        }
    }

    /// The wall-clock bound on one rsync.
    pub fn rsync_timeout(&self) -> std::time::Duration {
        let secs = self
            .home
            .rsync_timeout_secs
            .unwrap_or(DEFAULT_RSYNC_TIMEOUT_SECS);
        std::time::Duration::from_secs(secs.max(1))
    }

    /// Data connections per unit: the lower of this leaf's `[home] streams`
    /// and home's `[daemon] streams`, at least 1.
    pub fn streams(&self, home: u32) -> u32 {
        self.home.streams.unwrap_or(home).min(home).max(1)
    }

    /// Home's bundle on this machine.
    pub fn credentials(&self, dirs: &super::Dirs) -> PathBuf {
        match &self.home.credentials {
            Some(c) if Path::new(c).is_absolute() => PathBuf::from(c),
            Some(c) => dirs.root.join(c),
            None => dirs.home_bundle(),
        }
    }

    /// Where a normalized home path lives on this machine: the longest `--as`
    /// that covers it, else the identity mapping under `home_dir`.
    pub fn local_path(&self, rel: &str, home_dir: &Path) -> PathBuf {
        let best = self
            .as_local
            .iter()
            .filter(|(k, _)| paths::is_within(rel, k))
            .max_by_key(|(k, _)| k.len());
        match best {
            Some((k, base)) => match paths::relative_to(rel, k) {
                Some("") | None => base.clone(),
                Some(rest) => base.join(rest),
            },
            None => home_dir.join(rel),
        }
    }
}

/// An `--as` target: absolute, no `..`.
pub fn check_local(p: &Path) -> Result<()> {
    let s = p.to_string_lossy();
    if !p.is_absolute() || paths::has_dotdot(&s) {
        return Err(Error::Path(format!(
            "--as {s} must be an absolute path with no `..`"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> LeafConfig {
        LeafConfig {
            name: Some("birchbook".into()),
            home_dir: None,
            home: HomeEntry {
                name: "thistledown".into(),
                address: "thistledown:7750".into(),
                credentials: None,
                rsync_host: None,
                rsync_timeout_secs: None,
                streams: None,
            },
            as_local: BTreeMap::from([(
                "work/tundra".into(),
                PathBuf::from("/Users/b/work2/tundra"),
            )]),
        }
    }

    #[test]
    fn local_path_identity_and_as_override() {
        let c = cfg();
        let hd = Path::new("/Users/b");
        assert_eq!(
            c.local_path("work/sequoia", hd),
            PathBuf::from("/Users/b/work/sequoia")
        );
        assert_eq!(
            c.local_path("work/tundra", hd),
            PathBuf::from("/Users/b/work2/tundra")
        );
        assert_eq!(
            c.local_path("work/tundra/beacon", hd),
            PathBuf::from("/Users/b/work2/tundra/beacon")
        );
        assert_eq!(
            c.local_path("work/tundra2", hd),
            PathBuf::from("/Users/b/work/tundra2")
        );
    }

    #[test]
    fn save_load_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("leaf.toml");
        assert_eq!(LeafConfig::load(&p).unwrap(), None);
        cfg().save(&p).unwrap();
        assert_eq!(LeafConfig::load(&p).unwrap(), Some(cfg()));
    }

    #[test]
    fn rsync_host_defaults_to_home_name_and_local_means_none() {
        let mut c = cfg();
        assert_eq!(c.rsync_host().as_deref(), Some("thistledown"));
        c.home.rsync_host = Some("tc.tailnet".into());
        assert_eq!(c.rsync_host().as_deref(), Some("tc.tailnet"));
        c.home.rsync_host = Some(LOCAL_RSYNC.into());
        assert_eq!(c.rsync_host(), None);
    }

    #[test]
    fn rsync_timeout_defaults_and_floors_at_one_second() {
        let mut c = cfg();
        assert_eq!(
            c.rsync_timeout(),
            std::time::Duration::from_secs(DEFAULT_RSYNC_TIMEOUT_SECS)
        );
        c.home.rsync_timeout_secs = Some(42);
        assert_eq!(c.rsync_timeout(), std::time::Duration::from_secs(42));
        c.home.rsync_timeout_secs = Some(0);
        assert_eq!(c.rsync_timeout(), std::time::Duration::from_secs(1));
    }

    #[test]
    fn rsync_timeout_reads_from_leaf_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("leaf.toml");
        std::fs::write(
            &p,
            "[home]\nname=\"h\"\naddress=\"h:1\"\nrsync_timeout_secs = 7\n",
        )
        .unwrap();
        let c = LeafConfig::load(&p).unwrap().unwrap();
        assert_eq!(c.rsync_timeout(), std::time::Duration::from_secs(7));
    }

    #[test]
    fn streams_are_the_lower_of_both_sides_and_at_least_one() {
        let mut c = cfg();
        assert_eq!(c.streams(4), 4, "home's, by default");
        assert_eq!(c.streams(0), 1);
        c.home.streams = Some(2);
        assert_eq!(c.streams(4), 2);
        c.home.streams = Some(8);
        assert_eq!(c.streams(4), 4, "a leaf cannot widen home's");
        c.home.streams = Some(0);
        assert_eq!(c.streams(4), 1);
    }

    #[test]
    fn credentials_resolve_beside_leaf_toml() {
        let dirs = super::super::Dirs::at("/r".into());
        let mut c = cfg();
        assert_eq!(c.credentials(&dirs), PathBuf::from("/r/home.pem"));
        c.home.credentials = Some("tc.pem".into());
        assert_eq!(c.credentials(&dirs), PathBuf::from("/r/tc.pem"));
        c.home.credentials = Some("/abs/tc.pem".into());
        assert_eq!(c.credentials(&dirs), PathBuf::from("/abs/tc.pem"));
    }

    #[test]
    fn bad_as_entries_are_refused_on_load() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("leaf.toml");
        std::fs::write(
            &p,
            "[home]\nname=\"h\"\naddress=\"h:1\"\n[as]\n\"work/x\" = \"rel/path\"\n",
        )
        .unwrap();
        assert!(LeafConfig::load(&p).is_err());
        std::fs::write(
            &p,
            "[home]\nname=\"h\"\naddress=\"h:1\"\n[as]\n\"../x\" = \"/abs\"\n",
        )
        .unwrap();
        assert!(LeafConfig::load(&p).is_err());
    }
}
