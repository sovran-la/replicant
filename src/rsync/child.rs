//! Running rsync under a wall-clock bound.
//!
//! rsync is started in its own process group, so the ssh it spawns (and
//! anything ssh spawns) is in that group too. On timeout the whole group is
//! killed: killing rsync alone could leave a wedged ssh holding home's side
//! open.

use crate::{Error, Result};
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How often the child is checked for exit.
const POLL: Duration = Duration::from_millis(20);

/// What a finished child left behind.
#[derive(Debug)]
pub(super) struct Finished {
    /// `None`: killed by a signal.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

fn reader(mut pipe: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    rx
}

fn kill_group(pgid: u32) {
    // SAFETY: kill(2) with a negative pid signals a process group; no memory
    // is involved. The group is the child's own (process_group(0) below).
    unsafe {
        libc::kill(-(pgid as libc::pid_t), libc::SIGKILL);
    }
}

/// Run `program args`, killing its process group if it, or its output, is not
/// finished within `timeout`.
pub(super) fn run(program: &str, args: &[String], timeout: Duration) -> Result<Finished> {
    let deadline = Instant::now() + timeout;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| Error::Rsync(format!("could not run {program}: {e}")))?;
    let pgid = child.id();
    let out = reader(child.stdout.take().expect("stdout is piped"));
    let err = reader(child.stderr.take().expect("stderr is piped"));
    let timed_out = || {
        Error::Rsync(format!(
            "{program} {} did not finish within {}s; killed it",
            args.join(" "),
            timeout.as_secs()
        ))
    };
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if Instant::now() >= deadline {
            kill_group(pgid);
            let _ = child.wait();
            return Err(timed_out());
        }
        std::thread::sleep(POLL);
    };
    // rsync has exited, but something it left in its group (a persisted ssh
    // master) may still hold a pipe open. The bound covers that wait too.
    let collect = |rx: mpsc::Receiver<Vec<u8>>| {
        let left = deadline.saturating_duration_since(Instant::now());
        rx.recv_timeout(left).map_err(|_| {
            kill_group(pgid);
            timed_out()
        })
    };
    let stdout = collect(out)?;
    let stderr = collect(err)?;
    Ok(Finished {
        code: status.code(),
        stdout,
        stderr,
    })
}
