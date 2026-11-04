//! Where replicant keeps things, and the two configs it reads.
//!
//! One directory per machine, `$HOME/.sovran/replicant` unless
//! `REPLICANT_HOME` points elsewhere (every test does). Its contents decide the
//! role: a directory with `fleet.toml` is **home**; anything else is a leaf.
//!
//! | file | role | written by |
//! |---|---|---|
//! | `fleet.toml` | home | Brandon, plus `subscribe`/`approve` through [`edit`] |
//! | `leaf.toml` | leaf | `subscribe` |
//! | `ca.key`, `ca.pem`, `server.pem`, `credentials.pem` | home | the daemon, first start |
//! | `state/issued/<host>.pem` | home | `approve`; deleted when the host collects it |
//! | `home.pem` | leaf | Brandon, copied from home's `config export-creds` (bootstrap) |
//! | `host.pem` | leaf | the daemon, collected from home after `approve` |
//! | `replicant.sock`, `daemon.lock` | both | the daemon |
//! | `journal.log` | both | the daemon (home: everything; leaf: its own lines) |
//! | `state/` | both | the daemon: contacts, markers, baselines, push manifests |
//! | `backup/` | home | the daemon: what each push replaced or deleted |

pub mod addresses;
pub mod edit;
pub mod fleet;
pub mod leaf;

pub use addresses::Addresses;
pub use fleet::{Fleet, HostEntry, Pending, Root, Subscription};
pub use leaf::{HomeEntry, LeafConfig};

use crate::Result;
use std::path::{Path, PathBuf};

/// The environment variable that relocates the whole directory.
pub const HOME_ENV: &str = "REPLICANT_HOME";

/// The port a daemon listens on when an address names none.
pub const DEFAULT_PORT: u16 = 7750;

/// This machine's replicant directory and the well-known files inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirs {
    pub root: PathBuf,
}

impl Dirs {
    /// The directory for this process: `REPLICANT_HOME`, else
    /// `$HOME/.sovran/replicant`, beside the other sovran daemons.
    pub fn discover() -> Result<Self> {
        if let Some(dir) = std::env::var_os(HOME_ENV).filter(|v| !v.is_empty()) {
            return Ok(Self::at(PathBuf::from(dir)));
        }
        Ok(Self::at(user_home()?.join(".sovran").join("replicant")))
    }

    pub fn at(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn fleet(&self) -> PathBuf {
        self.root.join("fleet.toml")
    }
    pub fn leaf(&self) -> PathBuf {
        self.root.join("leaf.toml")
    }
    pub fn socket(&self) -> PathBuf {
        self.root.join("replicant.sock")
    }
    pub fn lock(&self) -> PathBuf {
        self.root.join("daemon.lock")
    }
    pub fn journal(&self) -> PathBuf {
        self.root.join("journal.log")
    }
    pub fn state(&self) -> PathBuf {
        self.root.join("state")
    }
    /// Home's push backups: `backup/<host>/<YYYYMMDDTHHMMSSZ>/`.
    pub fn backup(&self) -> PathBuf {
        self.root.join("backup")
    }
    pub fn credentials(&self) -> PathBuf {
        self.root.join("credentials.pem")
    }
    pub fn ca_key(&self) -> PathBuf {
        self.root.join("ca.key")
    }
    pub fn ca_cert(&self) -> PathBuf {
        self.root.join("ca.pem")
    }
    /// Home's server identity. Never leaves home.
    pub fn server_identity(&self) -> PathBuf {
        self.root.join("server.pem")
    }
    /// Where a leaf looks for home's bootstrap bundle when leaf.toml names
    /// none. Kept after enrollment: its CA is what the leaf trusts.
    pub fn home_bundle(&self) -> PathBuf {
        self.root.join("home.pem")
    }
    /// A leaf's own client identity, collected from home after `approve`.
    pub fn host_identity(&self) -> PathBuf {
        self.root.join("host.pem")
    }
    /// Home's issued, not yet collected, host identities.
    pub fn issued(&self) -> PathBuf {
        self.state().join("issued")
    }

    /// Whether this machine is home.
    pub fn is_home(&self) -> bool {
        self.fleet().is_file()
    }
}

/// `$HOME`, which must be set and absolute.
///
/// Never in a test (review N3): every fixture names its own home dir
/// (`Bed`, `test_certs::pair`), and one that forgot would otherwise read,
/// and through a sync write, the real one.
#[cfg(test)]
pub fn user_home() -> Result<PathBuf> {
    panic!(
        "user_home() called in a test: the fixture must set its own home_dir (leaf.toml \
         home_dir, fleet.toml home_dir, or REPLICANT_HOME), never fall through to $HOME"
    )
}

/// `$HOME`, which must be set and absolute.
#[cfg(not(test))]
pub fn user_home() -> Result<PathBuf> {
    match std::env::var_os("HOME") {
        Some(h) if Path::new(&h).is_absolute() => Ok(PathBuf::from(h)),
        _ => Err(crate::Error::Config(
            "$HOME is not set to an absolute path, and replicant maps every path through it".into(),
        )),
    }
}

/// This machine's short name: the first label of gethostname(2), lowercased.
///
/// `birchbook.local` and `Birchbook` are both `birchbook`, which is what the
/// fleet calls it. leaf.toml's `name` overrides this.
pub fn hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for len - 1 bytes and gethostname writes at
    // most that many; the final byte stays 0 so the result is terminated.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len() - 1) };
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(0);
    let raw = String::from_utf8(buf[..end].to_vec()).ok()?;
    short_hostname(&raw)
}

/// The fleet name of a raw hostname, or `None` if it is not one.
pub fn short_hostname(raw: &str) -> Option<String> {
    let first = raw.trim().split('.').next()?.to_ascii_lowercase();
    let valid = !first.is_empty()
        && !first.starts_with('-')
        && first
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    valid.then_some(first)
}

/// `host` or `host:port`, with the default port applied.
pub fn with_port(address: &str) -> String {
    let has_port = match address.strip_prefix('[') {
        Some(bracketed) => bracketed.contains("]:"),
        None => matches!(address.rsplit_once(':'), Some((_, p)) if p.parse::<u16>().is_ok()),
    };
    if has_port {
        address.to_string()
    } else {
        format!("{address}:{DEFAULT_PORT}")
    }
}

/// Write a file by way of a temporary sibling and a rename, so a crash leaves
/// either the old contents or the new ones.
///
/// The temporary name is unique per call, not just per process: home's
/// request threads write the same contact record concurrently, and a shared
/// temporary would be renamed away under the second writer. Concurrent
/// writers each land whole; the last rename wins.
pub fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp.{}.{n}", std::process::id()));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
#[path = "user_home_tests.rs"]
mod user_home_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_hostname_strips_domain_and_case() {
        assert_eq!(short_hostname("Birchbook.local"), Some("birchbook".into()));
        assert_eq!(short_hostname("thistledown"), Some("thistledown".into()));
        assert_eq!(
            short_hostname("spark-2.tail1234.ts.net"),
            Some("spark-2".into())
        );
        assert_eq!(short_hostname(""), None);
        assert_eq!(short_hostname("-bad"), None);
        assert_eq!(short_hostname("sp ace"), None);
    }

    #[test]
    fn with_port_applies_default_only_when_missing() {
        assert_eq!(
            with_port("thistledown"),
            format!("thistledown:{DEFAULT_PORT}")
        );
        assert_eq!(with_port("thistledown:9000"), "thistledown:9000");
        assert_eq!(with_port("127.0.0.1:1"), "127.0.0.1:1");
        assert_eq!(with_port("[::1]:7"), "[::1]:7");
        assert_eq!(with_port("[::1]"), format!("[::1]:{DEFAULT_PORT}"));
    }

    #[test]
    fn dirs_role_follows_fleet_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = Dirs::at(tmp.path().to_path_buf());
        assert!(!dirs.is_home());
        std::fs::write(dirs.fleet(), "").unwrap();
        assert!(dirs.is_home());
    }

    #[test]
    fn write_atomic_creates_parents_and_replaces() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a/b/c.txt");
        write_atomic(&p, "one").unwrap();
        write_atomic(&p, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "two");
        let siblings: Vec<_> = std::fs::read_dir(p.parent().unwrap()).unwrap().collect();
        assert_eq!(siblings.len(), 1, "no temporary file left behind");
    }

    /// Two request threads touching one host's contact record at once (a
    /// leaf's heartbeat and its `State` call) must both succeed.
    #[test]
    fn concurrent_writers_of_one_file_do_not_collide() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("contacts/leaf.json");
        let start = std::sync::Barrier::new(8);
        std::thread::scope(|s| {
            let handles: Vec<_> = (0..8)
                .map(|i| {
                    let (p, start) = (&p, &start);
                    s.spawn(move || {
                        start.wait();
                        (0..200)
                            .map(|n| write_atomic(p, &format!("{i}.{n}")))
                            .collect::<Result<Vec<_>>>()
                    })
                })
                .collect();
            for h in handles {
                h.join().unwrap().unwrap();
            }
        });
        let siblings: Vec<_> = std::fs::read_dir(p.parent().unwrap()).unwrap().collect();
        assert_eq!(siblings.len(), 1, "no temporary file left behind");
    }
}
