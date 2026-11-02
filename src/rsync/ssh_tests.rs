//! The transport to home: ssh options, the wall-clock bound, and the kill.
//! A fake ssh stands in for the real one; rsync itself is the real client.

use super::*;
use crate::testutil;
use std::time::Instant;

fn over_ssh(leaf: &Path, home: &Path) -> Transfer {
    Transfer {
        direction: Direction::Pull,
        leaf_path: leaf.into(),
        home_path: home.into(),
        home_host: Some("thistledown".into()),
        is_file: false,
        excludes: Excludes::for_root(&[]).unwrap(),
        source_absent: false,
    }
}

fn runner_with(ssh: &Path, timeout: Duration) -> Runner {
    Runner {
        ssh: ssh.display().to_string(),
        ..Runner::new(timeout, Client::Rsync, "/nonexistent".into())
    }
}

#[test]
fn ssh_transfers_never_prompt_and_notice_a_dead_peer() {
    let x = over_ssh(Path::new("/l/w"), Path::new("/h/w"));
    let a = x.args(&testutil::runner(), Path::new("/e"));
    let e = a.iter().position(|s| s == "-e").expect("-e is passed");
    assert_eq!(
        a[e + 1],
        "ssh -o BatchMode=yes -o ConnectTimeout=10 -o ServerAliveInterval=15 -o ServerAliveCountMax=2"
    );
    assert!(e < a.len() - 2, "options come before the endpoints: {a:?}");
}

#[test]
fn local_transfers_pass_no_ssh() {
    let mut x = over_ssh(Path::new("/l/w"), Path::new("/h/w"));
    x.home_host = None;
    let a = x.args(&testutil::runner(), Path::new("/e"));
    assert!(
        !a.iter().any(|s| s == "-e" || s.contains("BatchMode")),
        "{a:?}"
    );
}

#[test]
fn the_fake_ssh_receives_the_options() {
    let tmp = tempfile::tempdir().unwrap();
    let argv = tmp.path().join("argv");
    let ssh = testutil::script(
        tmp.path(),
        "ssh",
        &format!("printf '%s\\n' \"$@\" > {}\nexit 1", argv.display()),
    );
    let x = over_ssh(&tmp.path().join("leaf"), Path::new("/h/w"));
    let err = x.dry_run(&runner_with(&ssh, Duration::from_secs(30)));
    assert!(err.is_err(), "the fake ssh is not a server");
    let got = std::fs::read_to_string(&argv).unwrap();
    let got: Vec<&str> = got.lines().collect();
    assert_eq!(
        &got[..9],
        [
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=2",
            "thistledown"
        ]
    );
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[test]
fn a_hung_ssh_is_killed_at_the_bound_with_its_process_group() {
    let tmp = tempfile::tempdir().unwrap();
    let pidfile = tmp.path().join("pid");
    let ssh = testutil::script(
        tmp.path(),
        "ssh",
        &format!("echo $$ > {}\nexec sleep 60", pidfile.display()),
    );
    let x = over_ssh(&tmp.path().join("leaf"), Path::new("/h/w"));
    let started = Instant::now();
    let err = x
        .dry_run(&runner_with(&ssh, Duration::from_secs(1)))
        .unwrap_err()
        .to_string();
    let took = started.elapsed();
    assert!(err.contains("did not finish within 1s"), "{err}");
    assert!(err.contains("--dry-run"), "names the command: {err}");
    assert!(took >= Duration::from_secs(1), "{took:?}");
    assert!(took < Duration::from_secs(10), "bounded: {took:?}");
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SIGKILL is delivered at once; reaping by init can lag a moment.
    let gone_by = Instant::now() + Duration::from_secs(5);
    while alive(pid) && Instant::now() < gone_by {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!alive(pid), "the ssh rsync spawned was killed too");
}

#[test]
fn a_fast_failure_is_reported_well_inside_the_bound() {
    let tmp = tempfile::tempdir().unwrap();
    let ssh = testutil::script(tmp.path(), "ssh", "echo 'no route' >&2\nexit 255");
    let x = over_ssh(&tmp.path().join("leaf"), Path::new("/h/w"));
    let started = Instant::now();
    let err = x
        .dry_run(&runner_with(&ssh, Duration::from_secs(30)))
        .unwrap_err()
        .to_string();
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(err.contains("exited"), "{err}");
    assert!(!err.contains("did not finish"), "{err}");
}
