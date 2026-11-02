//! Review F3, second half: a lease lost while rsync runs stops that rsync.
//! The "rsync" is a script that records its pid and a grandchild's (in its
//! process group, as ssh would be) and sleeps; home drops the queue entry
//! once it is running.

use super::*;
use crate::daemon::home::Home;
use crate::testutil::{self, Bed, InProcess};
use std::time::{Duration, Instant};

/// A fake rsync that records its pid and a grandchild's, then sleeps.
/// (`Client::detect` still asks the real rsync for its version.)
pub(crate) fn sleeping_rsync(dir: &Path) -> String {
    let body = format!(
        "echo $$ > {p}\nsleep 30 &\necho $! > {g}\nwait\n",
        p = dir.join("rsync.pid").display(),
        g = dir.join("rsync.grandchild").display()
    );
    testutil::script(dir, "fake-rsync", &body)
        .display()
        .to_string()
}

pub(crate) fn wait_for_pid(p: &Path) -> i32 {
    let by = Instant::now() + Duration::from_secs(20);
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

/// Whether `pid` is gone (or a zombie) within a few seconds.
pub(crate) fn gone(pid: i32) -> bool {
    let by = Instant::now() + Duration::from_secs(5);
    while Instant::now() < by {
        // SAFETY: signal 0 delivers nothing.
        if unsafe { libc::kill(pid, 0) } != 0 {
            return true;
        }
        let out = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        let stat = String::from_utf8_lossy(&out.stdout);
        if stat.trim().is_empty() || stat.trim_start().starts_with('Z') {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

/// Drop whatever entry heads home's queue once `pid_file` exists.
pub(crate) fn drop_head_when_running<'s>(
    s: &'s std::thread::Scope<'s, '_>,
    home: &'s Home,
    pid_file: std::path::PathBuf,
) -> std::thread::ScopedJoinHandle<'s, ()> {
    s.spawn(move || {
        wait_for_pid(&pid_file);
        let head = home.queue.view().first().expect("an entry is held").id;
        home.queue.release(head).unwrap();
    })
}

#[test]
fn a_lost_lease_kills_the_running_rsync_and_fails_the_plan() {
    let bed = Bed::enrolled();
    testutil::hand_edit(
        &bed.home_dirs.fleet(),
        "listen = ",
        "lease_secs = 1\nlisten = ",
    );
    testutil::repo(&bed.leaf_hd.join("work/r"));
    let home = bed.home();
    let fake = sleeping_rsync(bed.tmp.path());
    let link = InProcess {
        home: &home,
        host: "leaf".into(),
        home_dir: bed.leaf_hd.clone(),
    };
    let cfg = bed.leaf_cfg();
    let (state, scratch) = (bed.leaf_dirs.state(), bed.leaf_dirs.scratch());
    let ctx = LeafCtx {
        me: "leaf",
        home_dir: &bed.leaf_hd,
        cfg: &cfg,
        state_dir: &state,
        scratch_dir: &scratch,
        rsync: &fake,
    };
    let started = Instant::now();
    let err = std::thread::scope(|s| {
        let dropper = drop_head_when_running(s, &home, bed.tmp.path().join("rsync.pid"));
        let r = plan(&link, ctx, true, false);
        dropper.join().unwrap();
        r
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("lease lost"), "{err}");
    // The script sleeps 30s; it was stopped, not waited out.
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
    for f in ["rsync.pid", "rsync.grandchild"] {
        let pid = wait_for_pid(&bed.tmp.path().join(f));
        assert!(gone(pid), "{f} {pid} survived the lost lease");
    }
    assert!(home.queue.view().is_empty());
}
