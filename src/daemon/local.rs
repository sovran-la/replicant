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
//! rsync (the unit left interrupted, its marker kept) and starts no other
//! unit (review N1).

use super::remote::{recv, recv_buffered, send};
use super::wire::{
    LOCAL_WIRE_VERSION, LocalAnswer, LocalCall, LocalFrame, LocalReply, LocalRequest,
};
use crate::config::Dirs;
use crate::plan::apply::{Progress, ProgressEvent};
use crate::rsync::Abort;
use crate::{Error, Result};
use std::io::{BufRead, BufReader, ErrorKind, Read};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Why a request was stopped when the CLI went away.
pub const HANGUP: &str = "the CLI hung up (Ctrl-C, or it was killed)";

/// How often the watch on the CLI's end looks up to see whether the request
/// is over (it is normally woken at once instead).
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

/// Serve one connection with `handle`.
pub fn serve(mut s: UnixStream, handle: Handler<'_>) {
    let answer: LocalAnswer = match recv::<_, LocalCall>(&mut s) {
        Ok(c) if c.version == LOCAL_WIRE_VERSION => watched(&s, |p| handle(c.request, p)),
        Ok(c) => Err(format!(
            "the CLI speaks wire version {} and this daemon {LOCAL_WIRE_VERSION}; restart the daemon after upgrading",
            c.version
        )),
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
    std::thread::scope(|scope| {
        scope.spawn(|| watch_hangup(s, &over, &abort));
        let out = f(&Progress::new(&emit, abort.clone()));
        over.store(true, Ordering::SeqCst);
        // Wakes the watch's read; the answer still goes out.
        let _ = s.shutdown(Shutdown::Read);
        out
    })
}

/// Read the CLI's end, which sends nothing after its request, until the
/// request is `over` or the CLI hangs up.
fn watch_hangup(s: &UnixStream, over: &AtomicBool, abort: &Abort) {
    // Only a fallback for the wake below; it fails on a socket whose peer
    // is already gone, and the read then says so at once.
    let _ = s.set_read_timeout(Some(WATCH_POLL));
    let mut r = s;
    let mut buf = [0u8; 64];
    while !over.load(Ordering::SeqCst) {
        let gone = match r.read(&mut buf) {
            Ok(0) => true,
            // Nothing is expected; something is not a hangup.
            Ok(_) => false,
            Err(e) => !matches!(
                e.kind(),
                ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
            ),
        };
        if gone {
            if !over.load(Ordering::SeqCst) {
                abort.raise_because(HANGUP);
            }
            return;
        }
    }
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
