//! The child runner against scripts: normal exit, the wall-clock bound, and
//! an abort raised from another thread, including a child that ignores
//! SIGTERM and a grandchild left in the group.

use super::*;
use crate::testutil;
use std::path::Path;

/// Whether a process still exists (a zombie counts as gone: it is not
/// running anything).
fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 checks for existence and delivers nothing.
    let exists = unsafe { libc::kill(pid, 0) } == 0;
    if !exists {
        return false;
    }
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let stat = String::from_utf8_lossy(&out.stdout);
    !stat.trim().is_empty() && !stat.trim_start().starts_with('Z')
}

fn gone_within(pid: i32, d: Duration) -> bool {
    let by = Instant::now() + d;
    while Instant::now() < by {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

fn read_pid(p: &Path) -> i32 {
    let by = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(t) = std::fs::read_to_string(p)
            && let Ok(n) = t.trim().parse()
        {
            return n;
        }
        assert!(Instant::now() < by, "{} never written", p.display());
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A script that prints a line, records its pid and a grandchild's, and
/// sleeps. `trap` decides what it does with SIGTERM.
fn sleeper(dir: &Path, trap: &str) -> String {
    let body = format!(
        "{trap}\necho started\nsleep 30 &\necho $! > {g}\necho $$ > {p}\nwait\n",
        g = dir.join("grandchild").display(),
        p = dir.join("pid").display()
    );
    testutil::script(dir, "fake-rsync", &body)
        .display()
        .to_string()
}

#[test]
fn a_normal_exit_reports_code_and_output() {
    let tmp = tempfile::tempdir().unwrap();
    let s = testutil::script(tmp.path(), "s", "echo out; echo err >&2; exit 3");
    let f = run(
        s.to_str().unwrap(),
        &[],
        Duration::from_secs(10),
        &Abort::default(),
    )
    .unwrap();
    assert_eq!(f.code, Some(3));
    assert_eq!(f.stopped, None);
    assert_eq!(String::from_utf8_lossy(&f.stdout), "out\n");
    assert_eq!(String::from_utf8_lossy(&f.stderr), "err\n");
}

#[test]
fn the_bound_kills_the_group() {
    let tmp = tempfile::tempdir().unwrap();
    let s = sleeper(tmp.path(), "");
    let f = run(&s, &[], Duration::from_millis(700), &Abort::default()).unwrap();
    assert_eq!(f.stopped, Some(Stopped::TimedOut));
    assert_eq!(f.code, None, "killed by a signal");
    for who in ["pid", "grandchild"] {
        let pid = read_pid(&tmp.path().join(who));
        assert!(gone_within(pid, Duration::from_secs(5)), "{who} {pid}");
    }
}

#[test]
fn an_abort_from_another_thread_stops_the_group_and_keeps_the_output() {
    let tmp = tempfile::tempdir().unwrap();
    let s = sleeper(tmp.path(), "");
    let abort = Abort::default();
    let raiser = {
        let abort = abort.clone();
        let pid_file = tmp.path().join("pid");
        std::thread::spawn(move || {
            read_pid(&pid_file);
            abort.raise();
        })
    };
    let started = Instant::now();
    let f = run(&s, &[], Duration::from_secs(60), &abort).unwrap();
    raiser.join().unwrap();
    assert_eq!(f.stopped, Some(Stopped::Aborted));
    assert!(
        started.elapsed() < GRACE,
        "SIGTERM was enough: {:?}",
        started.elapsed()
    );
    assert_eq!(String::from_utf8_lossy(&f.stdout), "started\n");
    for who in ["pid", "grandchild"] {
        let pid = read_pid(&tmp.path().join(who));
        assert!(gone_within(pid, Duration::from_secs(5)), "{who} {pid}");
    }
}

#[test]
fn a_child_that_ignores_sigterm_is_killed_after_the_grace() {
    let tmp = tempfile::tempdir().unwrap();
    let s = sleeper(tmp.path(), "trap '' TERM");
    let abort = Abort::default();
    let raiser = {
        let abort = abort.clone();
        let pid_file = tmp.path().join("pid");
        std::thread::spawn(move || {
            read_pid(&pid_file);
            abort.raise();
        })
    };
    let started = Instant::now();
    let f = run(&s, &[], Duration::from_secs(60), &abort).unwrap();
    raiser.join().unwrap();
    let took = started.elapsed();
    assert_eq!(f.stopped, Some(Stopped::Aborted));
    assert!(took >= GRACE, "waited out the grace: {took:?}");
    assert!(took < GRACE + Duration::from_secs(5), "{took:?}");
    for who in ["pid", "grandchild"] {
        let pid = read_pid(&tmp.path().join(who));
        assert!(gone_within(pid, Duration::from_secs(5)), "{who} {pid}");
    }
}

#[test]
fn an_abort_raised_before_the_start_stops_at_once() {
    let tmp = tempfile::tempdir().unwrap();
    let s = sleeper(tmp.path(), "");
    let abort = Abort::default();
    abort.raise();
    let f = run(&s, &[], Duration::from_secs(60), &abort).unwrap();
    assert_eq!(f.stopped, Some(Stopped::Aborted));
}

#[test]
fn an_abort_keeps_the_first_raiser_s_reason() {
    let a = Abort::default();
    assert!(!a.raised() && a.why().is_none());
    a.raise_because("the CLI hung up");
    a.raise_because("something later");
    a.raise();
    assert!(a.raised());
    assert_eq!(a.why().as_deref(), Some("the CLI hung up"), "first wins");
    assert_eq!(a.clone().why(), a.why(), "clones share it");

    let lease = Abort::default();
    lease.raise();
    lease.raise_because("the CLI hung up");
    assert_eq!(
        lease.why(),
        None,
        "a reasonless first raise is not relabeled"
    );
}
