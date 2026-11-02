//! The CLI's way in: a Unix socket in the replicant directory, one request
//! per connection, the same JSON-line framing as the mTLS transport.
//!
//! The socket is mode 0600 inside a 0700 directory: only this user reaches it,
//! which is the same boundary as the files the daemon reads and writes.
//!
//! The answer is a stream ([`LocalFrame`]): a sync that applies sends one
//! frame per unit started and finished, then the report.

use super::remote::{recv, recv_buffered, send};
use super::wire::{
    LOCAL_WIRE_VERSION, LocalAnswer, LocalCall, LocalFrame, LocalReply, LocalRequest,
};
use crate::config::Dirs;
use crate::plan::apply::{Progress, ProgressEvent};
use crate::{Error, Result};
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;

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

/// Run `f` with progress written to `s` as frames.
fn watched<T>(s: &UnixStream, f: impl FnOnce(&Progress<'_>) -> T) -> T {
    let emit = |e: ProgressEvent| {
        let mut w = s;
        if let Err(e) = send(&mut w, &LocalFrame::Progress(e)) {
            eprintln!("replicant: could not send the CLI progress: {e}");
        }
    };
    f(&Progress::new(&emit))
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
