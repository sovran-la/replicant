//! Fixture builders shared by the tests. Never compiled into the binary.
//!
//! Every fixture lives in a tempdir. git runs with the user's and system's
//! config switched off, so a hook or signing setting on the dev machine cannot
//! change what a test sees.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

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

/// An executable `sh` script at `dir/name`.
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

/// Aborts the test binary, naming the test, if this guard is still alive
/// after `limit`: for a call `bounded` cannot wrap because it borrows the
/// bed (a sync). A transfer that neither finishes nor fails would otherwise
/// hang the suite with no name on it (the A21 hang looked like that). The
/// guard ends with its scope, a panic included, so a test that has already
/// failed by name is not then blamed for a hang (review A43).
pub struct Watchdog {
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

pub fn watchdog(limit: Duration, what: String) -> Watchdog {
    let name = std::thread::current()
        .name()
        .unwrap_or("<unnamed test>")
        .to_string();
    let (stop, rx) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::spawn(move || {
        if rx.recv_timeout(limit) == Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
            eprintln!(
                "{name}: {what} is still running after {limit:?}: a stuck transfer; \
                 aborting the test binary so it fails by name instead of hanging"
            );
            std::process::abort();
        }
    });
    Watchdog {
        stop: Some(stop),
        thread: Some(thread),
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Owner-writable again, all of it: a tempdir cleans up only what it can.
/// A test that leaves a read-only directory anywhere under the bed (a
/// rollback that restores one, a backup of one) would otherwise leave the
/// bed behind (review A39).
pub fn open_all(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()) {
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755));
        if let Ok(entries) = std::fs::read_dir(p) {
            for e in entries.flatten() {
                open_all(&e.path());
            }
        }
    }
}

impl Drop for Bed {
    fn drop(&mut self) {
        open_all(self.tmp.path());
    }
}

impl Bed {
    /// `hosts` is extra fleet.toml text (hosts, subscriptions).
    pub fn new(hosts: &str) -> Self {
        // Both sides of every connection live in this process (review A42).
        static FDS: std::sync::Once = std::sync::Once::new();
        FDS.call_once(|| {
            crate::daemon::raise_open_files();
        });
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
                address: crate::config::Addresses::one("127.0.0.1:1"),
                credentials: None,
                rsync_host: None,
                rsync_timeout_secs: None,
                streams: None,
            },
            as_local: Default::default(),
        }
    }
}

/// Rewrite fleet.toml in place (same inode, as `>` or an editor that does not
/// rename would) and move its mtime a full second, so a running `Home` sees
/// the edit whatever the filesystem's timestamp granularity.
pub fn hand_edit(fleet: &Path, from: &str, to: &str) {
    let text = std::fs::read_to_string(fleet).unwrap();
    assert!(text.contains(from), "{from:?} not in fleet.toml:\n{text}");
    let was = std::fs::metadata(fleet).unwrap().modified().unwrap();
    let f = std::fs::File::options()
        .write(true)
        .truncate(true)
        .open(fleet)
        .unwrap();
    std::io::Write::write_all(&mut &f, text.replace(from, to).as_bytes()).unwrap();
    f.set_modified(was + std::time::Duration::from_secs(1))
        .unwrap();
}

/// Wait until the wall clock is in a later whole second than when called, so
/// a marker written after this starts strictly after every file written
/// before it (markers keep whole seconds; guardrail 6 counts that second).
pub fn next_second() {
    let start = crate::now_secs();
    while crate::now_secs() == start {
        let into = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        std::thread::sleep(std::time::Duration::from_nanos(
            u64::from(1_000_000_000 - into) + 1_000_000,
        ));
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

    fn open_data(&self, _rel: &str) -> crate::Result<crate::plan::DataLink<'_>> {
        let (conn, home_end) = crate::native::conn::Conn::pair()?;
        let home = self.home;
        Ok(crate::plan::DataLink {
            conn,
            serve: Some(Box::new(move || {
                home.serve_data(&crate::daemon::home::Caller::Trusted, home_end)
            })),
        })
    }
}

/// A hello for `rel` as `leaf`, with a short ack timeout.
pub fn hello(
    lease: u64,
    rel: &str,
    direction: crate::marker::Direction,
) -> crate::native::session::Hello {
    crate::native::session::Hello {
        version: crate::native::frame::DATA_VERSION,
        host: "leaf".into(),
        lease,
        rel: rel.into(),
        kind: crate::units::UnitKind::Loose,
        is_file: false,
        direction,
        ack_timeout_ms: 2000,
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
