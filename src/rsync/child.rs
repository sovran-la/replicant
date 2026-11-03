//! Running rsync under a wall-clock bound, and stopping it on request.
//!
//! rsync is started in its own process group, so the ssh it spawns (and
//! anything ssh spawns) is in that group too. On timeout the whole group is
//! killed: killing rsync alone could leave a wedged ssh holding home's side
//! open.
//!
//! An [`Abort`] raised by another thread (the lease heartbeat, when home says
//! the queue entry is gone) stops the group the same way, politely first:
//! SIGTERM, then SIGKILL after [`GRACE`]. The signals are sent by the thread
//! that waits on the child, never by the one that raised the flag: only the
//! waiter knows whether rsync is still running. (The SIGKILL that follows an
//! rsync which did exit in its grace is for what it left in its group; the
//! group id stays reserved while anything is left in it, as the pipe-drain
//! kill below has always assumed.)

pub use crate::abort::Abort;
use crate::{Error, Result};
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How often the child is checked for exit (and the abort flag read).
const POLL: Duration = Duration::from_millis(20);

/// How long a stopped rsync gets between SIGTERM and SIGKILL: long enough to
/// remove its temporary file, short enough not to hold home's queue.
pub(super) const GRACE: Duration = Duration::from_secs(2);

/// How long output is still collected after the group was killed.
const DRAIN: Duration = Duration::from_secs(5);

/// Why a child did not finish on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// Past the wall-clock bound.
    TimedOut,
    /// Its [`Abort`] was raised.
    Aborted,
}

/// What a child left behind.
#[derive(Debug)]
pub(super) struct Finished {
    /// `None`: killed by a signal.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// Set when replicant stopped it.
    pub stopped: Option<Stopped>,
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

fn signal_group(pgid: u32, sig: libc::c_int) {
    // SAFETY: kill(2) with a negative pid signals a process group; no memory
    // is involved. The group is the child's own (process_group(0) below).
    unsafe {
        libc::kill(-(pgid as libc::pid_t), sig);
    }
}

/// Run `program args`, stopping its process group if it is not finished
/// within `timeout` or `abort` is raised. A stop is not an error here: the
/// caller gets what the child printed until then and decides.
pub(super) fn run(
    program: &str,
    args: &[String],
    timeout: Duration,
    abort: &Abort,
) -> Result<Finished> {
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
    let mut stopped = None;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if abort.raised() {
            stopped = Some(Stopped::Aborted);
            signal_group(pgid, libc::SIGTERM);
            let by = Instant::now() + GRACE;
            let mut done = None;
            while Instant::now() < by {
                if let Some(s) = child.try_wait()? {
                    done = Some(s);
                    break;
                }
                std::thread::sleep(POLL);
            }
            // Whatever rsync left in its group (ssh) goes too.
            signal_group(pgid, libc::SIGKILL);
            break match done {
                Some(s) => s,
                None => child.wait()?,
            };
        }
        if Instant::now() >= deadline {
            stopped = Some(Stopped::TimedOut);
            signal_group(pgid, libc::SIGKILL);
            break child.wait()?;
        }
        std::thread::sleep(POLL);
    };
    // rsync has exited, but something it left in its group (a persisted ssh
    // master) may still hold a pipe open. The bound covers that wait too;
    // for an rsync that was stopped or died of a signal, whatever is left
    // gets only a short drain before the group goes.
    let by = match (stopped, status.code()) {
        (None, Some(_)) => deadline,
        _ => (Instant::now() + DRAIN).min(deadline.max(Instant::now())),
    };
    let mut collect = |rx: mpsc::Receiver<Vec<u8>>| {
        rx.recv_timeout(by.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|_| {
                signal_group(pgid, libc::SIGKILL);
                // An rsync that exited on its own, held open past the bound:
                // the bound's failure. One that died of a signal already
                // failed, and says so.
                if status.code().is_some() {
                    stopped.get_or_insert(Stopped::TimedOut);
                }
                Vec::new()
            })
    };
    let stdout = collect(out);
    let stderr = collect(err);
    Ok(Finished {
        code: status.code(),
        stdout,
        stderr,
        stopped,
    })
}

#[cfg(test)]
#[path = "child_tests.rs"]
mod tests;
