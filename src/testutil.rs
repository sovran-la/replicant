//! Fixture builders shared by the tests. Never compiled into the binary.
//!
//! Every fixture lives in a tempdir. git runs with the user's and system's
//! config switched off, so a hook or signing setting on the dev machine cannot
//! change what a test sees.

use std::path::{Path, PathBuf};
use std::process::Command;

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The rsync runner tests use: real ssh options, a bound no test reaches.
pub fn runner() -> crate::rsync::Runner {
    crate::rsync::Runner::new(std::time::Duration::from_secs(60))
}

/// An executable `sh` script at `dir/name`, for standing in for ssh.
pub fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

pub fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// A repo with one commit of `README`.
pub fn repo(path: &Path) -> PathBuf {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "-q"]);
    write(&path.join("README"), "hello\n");
    git(path, &["add", "README"]);
    git(path, &["commit", "-qm", "init"]);
    path.to_path_buf()
}

/// The fixture tree the task calls for, under `base`:
///
/// ```text
/// base/
///   alpha/            repo, clean
///   beta/             repo, dirty (modified + untracked), .git/index.lock held
///   nested/gamma/     repo
///   alpha-wt/         worktree of alpha (a .git *file* into alpha/.git/worktrees/)
///   notes.txt         loose
///   data/blob.bin     loose
///   target/junk       excluded by the work root's excludes
/// ```
pub fn tree(base: &Path) {
    let alpha = repo(&base.join("alpha"));
    let beta = repo(&base.join("beta"));
    repo(&base.join("nested/gamma"));
    git(
        &alpha,
        &["worktree", "add", "-q", "-b", "wt", "../alpha-wt"],
    );
    write(&beta.join("README"), "changed\n");
    write(&beta.join("new.txt"), "untracked\n");
    write(&beta.join(".git/index.lock"), "");
    write(&base.join("notes.txt"), "loose\n");
    write(&base.join("data/blob.bin"), "0123456789");
    write(&base.join("target/junk"), "build output");
}

/// A home and a leaf on one machine, in one tempdir, wired in-process.
///
/// ```text
/// tmp/home-cfg/fleet.toml   home's replicant dir (state, journal)
/// tmp/home-hd/work/          home's root `work`
/// tmp/leaf-cfg/             leaf's replicant dir (state, journal)
/// tmp/leaf-hd/work/          the leaf's identity mapping of `work`
/// ```
pub struct Bed {
    pub tmp: tempfile::TempDir,
    pub home_dirs: crate::config::Dirs,
    pub home_hd: PathBuf,
    pub leaf_dirs: crate::config::Dirs,
    pub leaf_hd: PathBuf,
}

impl Bed {
    /// `hosts` is extra fleet.toml text (hosts, subscriptions).
    pub fn new(hosts: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home_hd = tmp.path().join("home-hd");
        let leaf_hd = tmp.path().join("leaf-hd");
        std::fs::create_dir_all(home_hd.join("work")).unwrap();
        std::fs::create_dir_all(leaf_hd.join("work")).unwrap();
        let home_dirs = crate::config::Dirs::at(tmp.path().join("home-cfg"));
        let leaf_dirs = crate::config::Dirs::at(tmp.path().join("leaf-cfg"));
        std::fs::create_dir_all(&leaf_dirs.root).unwrap();
        let fleet = format!(
            "[daemon]\nname = \"home\"\nhome_dir = \"{}\"\nlisten = \"127.0.0.1:0\"\n\n\
             [roots.work]\npath = \"{}/work\"\nexclude = [\"target\"]\n\n{hosts}",
            home_hd.display(),
            home_hd.display()
        );
        write(&home_dirs.fleet(), &fleet);
        Self {
            tmp,
            home_dirs,
            home_hd,
            leaf_dirs,
            leaf_hd,
        }
    }

    /// The bed with `leaf` enrolled (no cap) and subscribed to `work`.
    pub fn enrolled() -> Self {
        let b = Self::new("");
        let extra = format!(
            "[hosts.leaf]\nhome_dir = \"{}\"\n\n[subscriptions.leaf]\npaths = [\"work\"]\n",
            b.leaf_hd.display()
        );
        let mut text = std::fs::read_to_string(b.home_dirs.fleet()).unwrap();
        text.push_str(&extra);
        write(&b.home_dirs.fleet(), &text);
        b
    }

    pub fn home(&self) -> crate::daemon::home::Home {
        crate::daemon::home::Home::open(self.home_dirs.clone()).unwrap()
    }

    pub fn leaf_cfg(&self) -> crate::config::LeafConfig {
        crate::config::LeafConfig {
            name: Some("leaf".into()),
            home_dir: Some(self.leaf_hd.clone()),
            home: crate::config::HomeEntry {
                name: "home".into(),
                address: "127.0.0.1:1".into(),
                credentials: None,
                rsync_host: Some(crate::config::leaf::LOCAL_RSYNC.into()),
                rsync_timeout_secs: None,
            },
            as_local: Default::default(),
        }
    }
}

/// A [`crate::plan::HomeLink`] that calls a [`crate::daemon::home::Home`]
/// directly, as `host`.
pub struct InProcess<'a> {
    pub home: &'a crate::daemon::home::Home,
    pub host: String,
    pub home_dir: PathBuf,
}

impl crate::plan::HomeLink for InProcess<'_> {
    fn call(
        &self,
        request: crate::daemon::wire::HomeRequest,
    ) -> crate::Result<crate::daemon::wire::HomeReply> {
        let call = crate::daemon::wire::HomeCall {
            version: crate::daemon::wire::WIRE_VERSION,
            host: self.host.clone(),
            home_dir: self.home_dir.clone(),
            request,
        };
        self.home.handle(call).result.map_err(crate::Error::Remote)
    }
}

/// Every file under `root` with its bytes and mode, for "nothing was written".
pub fn fingerprint(
    root: &Path,
) -> std::collections::BTreeMap<String, (Vec<u8>, u32, std::time::SystemTime)> {
    use std::os::unix::fs::PermissionsExt;
    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd {
            let p = e.unwrap().path();
            let m = std::fs::symlink_metadata(&p).unwrap();
            let rel = p.strip_prefix(root).unwrap().display().to_string();
            if m.is_dir() {
                out.insert(
                    format!("{rel}/"),
                    (vec![], m.permissions().mode(), m.modified().unwrap()),
                );
                stack.push(p);
            } else if m.is_file() {
                out.insert(
                    rel,
                    (
                        std::fs::read(&p).unwrap(),
                        m.permissions().mode(),
                        m.modified().unwrap(),
                    ),
                );
            }
        }
    }
    out
}
