//! The CLI's way in: a Unix socket in the replicant directory, one request
//! per connection, the same JSON-line framing as the mTLS transport.
//!
//! The socket is mode 0600 inside a 0700 directory: only this user reaches it,
//! which is the same boundary as the files the daemon reads and writes.

use super::remote::{recv, send};
use super::wire::{LocalAnswer, LocalCall, LocalReply, LocalRequest, WIRE_VERSION};
use crate::config::Dirs;
use crate::{Error, Result};
use std::os::unix::net::UnixStream;

/// Send one request to this machine's daemon.
pub fn call(dirs: &Dirs, request: LocalRequest) -> Result<LocalReply> {
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
            version: WIRE_VERSION,
            request,
        },
    )?;
    let answer: LocalAnswer = recv(&mut s)?;
    answer.map_err(Error::Daemon)
}

/// Serve one connection with `handle`.
pub fn serve(mut s: UnixStream, handle: &dyn Fn(LocalRequest) -> LocalAnswer) {
    let answer: LocalAnswer = match recv::<_, LocalCall>(&mut s) {
        Ok(c) if c.version == WIRE_VERSION => handle(c.request),
        Ok(c) => Err(format!(
            "the CLI speaks wire version {} and this daemon {WIRE_VERSION}; restart the daemon after upgrading",
            c.version
        )),
        Err(e) => Err(e.to_string()),
    };
    if let Err(e) = send(&mut s, &answer) {
        eprintln!("replicant: could not answer the CLI: {e}");
    }
}
