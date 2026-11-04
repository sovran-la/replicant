//! The CLI's way in: a Unix socket in the replicant directory, one request
//! per connection, the same JSON-line framing as the mTLS transport.
//!
//! The socket is mode 0600 inside a 0700 directory: only this user reaches it,
//! which is the same boundary as the files the daemon reads and writes.
//!
//! The answer is a stream ([`LocalFrame`]): a sync that applies sends one
//! frame per unit started and finished, then the report. While a request
//! runs, the daemon watches the CLI's end: once it hangs up (Ctrl-C, or the
//! CLI killed), the request's abort is raised, which stops a sync's running
//! transfer (the unit left interrupted, its marker kept) and starts no other
//! unit (review N1).

use super::remote::{recv_buffered, recv_versioned, send};
use super::wire::{
    LOCAL_WIRE_VERSION, LocalAnswer, LocalCall, LocalFrame, LocalReply, LocalRequest,
};
use crate::abort::Abort;
use crate::config::Dirs;
use crate::plan::apply::{Progress, ProgressEvent};
use crate::{Error, Result};
use std::io::{self, BufRead, BufReader, ErrorKind, PipeReader};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Why a request was stopped when the CLI went away.
pub const HANGUP: &str = "the CLI hung up (Ctrl-C, or it was killed)";

/// The longest the watch on the CLI's end sleeps before looking up to see
/// whether the request is over (it is normally woken at once instead).
const WATCH_POLL: Duration = Duration::from_millis(250);

/// Send one request to this machine's daemon.
pub fn call(dirs: &Dirs, request: LocalRequest) -> Result<LocalReply> {
    call_streaming(dirs, request, &mut |_| {})
}

/// [`call`], handing each progress frame to `on` as it arrives.
pub fn call_streaming(
    dirs: &Dirs,
    request: LocalRequest,
    on: &mut dyn FnMut(ProgressEvent),
) -> Result<LocalReply> {
    let socket = dirs.socket();
    let mut s = UnixStream::connect(&socket).map_err(|e| {
        Error::Daemon(format!(
            "no replicant daemon is answering at {} ({e}). Start one with `replicant daemon`",
            socket.display()
        ))
    })?;
    send(
        &mut s,
        &LocalCall {
            version: LOCAL_WIRE_VERSION,
            request,
        },
    )?;
    read_answer(&mut BufReader::new(s), on)
}

/// Read frames until the answer, handing progress to `on`.
pub fn read_answer(r: &mut impl BufRead, on: &mut dyn FnMut(ProgressEvent)) -> Result<LocalReply> {
    loop {
        match recv_buffered::<_, LocalFrame>(r)? {
            LocalFrame::Progress(e) => on(e),
            LocalFrame::Ok(reply) => return Ok(reply),
            LocalFrame::Err(e) => return Err(Error::Daemon(e)),
        }
    }
}

/// What serves a request: its answer, given where to send progress.
pub type Handler<'h> = &'h dyn Fn(LocalRequest, &Progress<'_>) -> LocalAnswer;

/// Serve one connection with `handle`. The version is checked before the
/// request is read, so a CLI of another version hears that, not a field it
/// shaped differently.
pub fn serve(mut s: UnixStream, handle: Handler<'_>) {
    let call = recv_versioned::<_, LocalCall>(&mut s, LOCAL_WIRE_VERSION, |v| {
        Error::Daemon(format!(
            "the CLI speaks wire version {v} and this daemon {LOCAL_WIRE_VERSION}; restart the \
             daemon after upgrading"
        ))
    });
    let answer: LocalAnswer = match call {
        Ok(c) => watched(&s, |p| handle(c.request, p)),
        Err(e) => Err(e.to_string()),
    };
    if let Err(e) = send(&mut s, &LocalFrame::from(answer)) {
        eprintln!("replicant: could not answer the CLI: {e}");
    }
}

/// Run `f` with progress written to `s` as frames, and an abort raised
/// once the CLI hangs up: its end closed or broken, or a frame that could
/// not be written.
fn watched<T>(s: &UnixStream, f: impl FnOnce(&Progress<'_>) -> T) -> T {
    let abort = Abort::default();
    let over = AtomicBool::new(false);
    let emit = |e: ProgressEvent| {
        let mut w = s;
        if send(&mut w, &LocalFrame::Progress(e)).is_err() {
            abort.raise_because(HANGUP);
        }
    };
    // Out of descriptors, there is no pipe: the watch still ends, within
    // WATCH_POLL.
    let (woken, wake) = io::pipe().ok().unzip();
    std::thread::scope(|scope| {
        scope.spawn(|| watch_hangup(s, woken.as_ref(), &over, &abort));
        let out = f(&Progress::new(&emit, abort.clone()));
        over.store(true, Ordering::SeqCst);
        // Closing the writer wakes the watch; so does `f` panicking, which
        // drops it too. The socket is untouched, and the answer still goes out.
        drop(wake);
        out
    })
}

/// Watch the CLI's end, which sends nothing after its request, until the
/// request is `over` or the CLI hangs up.
///
/// Never a blocking read with a timeout, woken by `shutdown`: on macOS such
/// a read on a Unix socket can stay blocked forever, if the `shutdown` lands
/// as the timeout expires, and `shutdown` fails once the peer has closed.
fn watch_hangup(s: &UnixStream, woken: Option<&PipeReader>, over: &AtomicBool, abort: &Abort) {
    let entry = |fd| libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let mut fds = [
        entry(s.as_raw_fd()),
        entry(woken.map_or(-1, |w| w.as_raw_fd())),
    ];
    let watching = 1 + libc::nfds_t::from(woken.is_some());
    while !over.load(Ordering::SeqCst) {
        // SAFETY: `fds` is live, and holds at least `watching` entries.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), watching, WATCH_POLL.as_millis() as _) };
        if ready < 0 {
            // Not word of the CLI. Out of kernel memory, say: look again.
            if io::Error::last_os_error().kind() != ErrorKind::Interrupted {
                std::thread::sleep(WATCH_POLL);
            }
        } else if fds[1].revents != 0 {
            return;
        } else if hung_up(s, fds[0].revents) {
            if !over.load(Ordering::SeqCst) {
                abort.raise_because(HANGUP);
            }
            return;
        }
    }
}

/// Whether `revents`, what `poll` said of the CLI's end, means it has gone.
fn hung_up(s: &UnixStream, revents: libc::c_short) -> bool {
    if revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
        return true;
    }
    if revents & libc::POLLIN == 0 {
        return revents & libc::POLLHUP != 0;
    }
    // Per call, not O_NONBLOCK: that would also hold for the frames going out
    // on this socket. And readable is no promise: this must not block.
    let mut buf = [0u8; 64];
    // SAFETY: `buf` is live and `buf.len()` long; `s` is an open socket.
    let n = unsafe {
        libc::recv(
            s.as_raw_fd(),
            buf.as_mut_ptr().cast(),
            buf.len(),
            libc::MSG_DONTWAIT,
        )
    };
    match n {
        0 => true,
        // Nothing is expected; something is not a hangup.
        1.. => false,
        _ => !matches!(
            io::Error::last_os_error().kind(),
            ErrorKind::WouldBlock | ErrorKind::Interrupted
        ),
    }
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
