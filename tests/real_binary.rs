//! The binary as shipped, two real daemons on loopback, under a throwaway
//! `REPLICANT_HOME` each.
//!
//! The unit suite runs with SIGPIPE ignored, as every libtest binary does,
//! so it cannot see a daemon die of a closed pipe. 0.4.6 as tagged did:
//! any failed transfer killed the leaf daemon (review A41). This is the
//! one test that runs the daemon with the disposition `main` gives it.

use replicant::config::{Addresses, Dirs, HomeEntry, LeafConfig};
use replicant::daemon::certs;
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn replicant(dirs: &Dirs) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_replicant"));
    c.env("REPLICANT_HOME", &dirs.root);
    c
}

/// A daemon that is killed when this is dropped, however the test ends.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn daemon(dirs: &Dirs) -> Running {
    Running(
        replicant(dirs)
            .arg("daemon")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}

fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let by = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < by, "{what}: not within 20 s");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn chmod(p: &Path, mode: u32) {
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// A port nobody listens on right now, for home's `listen`.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn a_failed_push_interrupts_the_sync_and_the_leaf_daemon_lives_on() {
    let tmp = tempfile::tempdir().unwrap();
    let (home_hd, leaf_hd) = (tmp.path().join("home-hd"), tmp.path().join("leaf-hd"));
    std::fs::create_dir_all(home_hd.join("work")).unwrap();
    std::fs::create_dir_all(leaf_hd.join("work")).unwrap();
    let home_dirs = Dirs::at(tmp.path().join("home-cfg"));
    let leaf_dirs = Dirs::at(tmp.path().join("leaf-cfg"));
    std::fs::create_dir_all(&home_dirs.root).unwrap();
    std::fs::create_dir_all(&leaf_dirs.root).unwrap();
    let port = free_port();
    std::fs::write(
        home_dirs.fleet(),
        format!(
            "[daemon]\nname = \"home\"\nhome_dir = \"{hd}\"\nlisten = \"127.0.0.1:{port}\"\n\n\
             [roots.work]\npath = \"{hd}/work\"\n\n\
             [hosts.leaf]\nhome_dir = \"{ld}\"\n\n\
             [subscriptions.leaf]\npaths = [\"work\"]\n",
            hd = home_hd.display(),
            ld = leaf_hd.display()
        ),
    )
    .unwrap();

    // Enrolled by hand, as approve would: home's CA, a certificate for the
    // leaf pinned in fleet.toml, the bootstrap bundle and that certificate
    // under the leaf's dirs.
    let p = certs::Paths::of(&home_dirs);
    certs::ensure(&p).unwrap();
    let issued = certs::issue(&p, "leaf").unwrap();
    replicant::config::edit::set_cert(&home_dirs.fleet(), "leaf", Some(&issued.fingerprint))
        .unwrap();
    std::fs::copy(&p.bootstrap, leaf_dirs.home_bundle()).unwrap();
    certs::write_secret(&leaf_dirs.host_identity(), &issued.bundle).unwrap();
    LeafConfig {
        name: Some("leaf".into()),
        home_dir: Some(leaf_hd.clone()),
        home: HomeEntry {
            name: "home".into(),
            address: Addresses::one(format!("127.0.0.1:{port}")),
            credentials: None,
            rsync_host: None,
            rsync_timeout_secs: None,
            streams: None,
        },
        as_local: Default::default(),
    }
    .save(&leaf_dirs.leaf())
    .unwrap();

    let _home = daemon(&home_dirs);
    until("home listening", || {
        TcpStream::connect(("127.0.0.1", port)).is_ok()
    });
    let mut leaf = daemon(&leaf_dirs);
    until("leaf answering", || {
        replicant(&leaf_dirs)
            .args(["status"])
            .output()
            .is_ok_and(|o| o.status.success())
    });

    // Something to push, into a directory home may not write.
    std::fs::write(leaf_hd.join("work/f.txt"), "from the leaf").unwrap();
    chmod(&home_hd.join("work"), 0o555);
    let out = replicant(&leaf_dirs)
        .args(["sync", "--yes", "--non-interactive"])
        .output()
        .unwrap();
    chmod(&home_hd.join("work"), 0o755);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "{text}");
    assert!(
        text.contains("interrupted") && text.contains("Permission denied"),
        "the failure is reported as an interruption, with its cause:\n{text}"
    );
    assert!(
        !text.contains("closed the connection without answering"),
        "that is what a dead leaf daemon looks like:\n{text}"
    );

    // The leaf daemon is still there, and still answers.
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        leaf.0.try_wait().unwrap().is_none(),
        "the leaf daemon exited"
    );
    let status = replicant(&leaf_dirs).args(["status"]).output().unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
}
