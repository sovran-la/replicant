//! Daemon-to-daemon: mTLS over plain TCP, one request per connection.
//!
//! The agents crate's pattern (`remote/listener.rs`, `remote/client.rs`)
//! without its MCP framing: replicant speaks one JSON line out and one JSON
//! line back (see [`super::wire`]), over the same rustls stack. Both the
//! local Unix socket and this transport use [`send`] and [`recv`].

use super::tls::Identity;
use crate::{Error, Result};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use serde::{Serialize, de::DeserializeOwned};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

/// A message larger than this is refused rather than buffered.
pub const MAX_MESSAGE: u64 = 64 << 20;

/// How long a peer may take to finish the TLS handshake.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Write one message as one line.
pub fn send<W: Write, T: Serialize>(w: &mut W, v: &T) -> Result<()> {
    let mut line = serde_json::to_vec(v).map_err(|e| Error::Remote(format!("encode: {e}")))?;
    line.push(b'\n');
    w.write_all(&line)?;
    w.flush()?;
    Ok(())
}

/// Read one line as one message.
pub fn recv<R: Read, T: DeserializeOwned>(r: R) -> Result<T> {
    recv_buffered(&mut BufReader::new(r))
}

/// [`recv`] from a reader kept across messages: whatever it buffered past
/// this line is the next message's, not lost with a throwaway buffer.
pub fn recv_buffered<R: BufRead, T: DeserializeOwned>(r: &mut R) -> Result<T> {
    parse(&read_line(r)?)
}

/// [`recv`], but the message's `version` field is compared with `expected`
/// before the rest is read: a message from another version, whose shape may
/// differ, then gets `mismatch`'s error, which says which versions disagree,
/// instead of "unreadable message" about some field.
pub fn recv_versioned<R: Read, T: DeserializeOwned>(
    r: R,
    expected: u32,
    mismatch: impl FnOnce(u32) -> Error,
) -> Result<T> {
    #[derive(serde::Deserialize)]
    struct Peek {
        version: u32,
    }
    let line = read_line(&mut BufReader::new(r))?;
    let peek: Peek = parse(&line)?;
    if peek.version != expected {
        return Err(mismatch(peek.version));
    }
    parse(&line)
}

fn read_line<R: BufRead>(r: &mut R) -> Result<Vec<u8>> {
    let mut line = Vec::new();
    r.take(MAX_MESSAGE).read_until(b'\n', &mut line)?;
    if line.is_empty() {
        return Err(Error::Remote(
            "the peer closed the connection without answering".into(),
        ));
    }
    if line.last() != Some(&b'\n') {
        return Err(Error::Remote(
            "message truncated or larger than the limit".into(),
        ));
    }
    Ok(line)
}

fn parse<T: DeserializeOwned>(line: &[u8]) -> Result<T> {
    serde_json::from_slice(line).map_err(|e| Error::Remote(format!("unreadable message: {e}")))
}

pub type ServerStream = StreamOwned<ServerConnection, TcpStream>;

/// Whether the peer of `sock` is still there, without consuming anything: a
/// non-blocking peek that would block means connected; end-of-file or an
/// error means it left (a dead process's socket is closed by its kernel).
/// Bytes waiting count as connected. A peer that vanished without a FIN (a
/// power cut, a partition) also reads as connected; nothing here can tell.
pub fn connected(sock: &TcpStream) -> bool {
    if sock.set_nonblocking(true).is_err() {
        return true;
    }
    let peeked = sock.peek(&mut [0u8; 1]);
    let restored = sock.set_nonblocking(false).is_ok();
    match peeked {
        Ok(0) => false,
        Ok(_) => restored,
        Err(e) => {
            restored
                && matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                )
        }
    }
}

/// The mTLS front door.
pub struct MtlsListener {
    tcp: TcpListener,
    addr: SocketAddr,
    config: Arc<ServerConfig>,
}

impl MtlsListener {
    /// Bind `address`, which must name one specific interface: a listener that
    /// answers on every interface is a reach nobody chose.
    pub fn bind(address: &str, identity: &Identity) -> Result<Self> {
        let config = identity.server_config()?;
        let resolved: Vec<SocketAddr> = address
            .to_socket_addrs()
            .map_err(|e| Error::Config(format!("[daemon] listen = {address:?}: {e}")))?
            .collect();
        let addr = match resolved.as_slice() {
            [one] if one.ip().is_unspecified() => {
                return Err(Error::Config(format!(
                    "[daemon] listen = {address:?} would answer on every interface; name this \
                     machine's tailnet address instead"
                )));
            }
            [one] => *one,
            _ => {
                return Err(Error::Config(format!(
                    "[daemon] listen = {address:?} must resolve to exactly one address"
                )));
            }
        };
        let tcp = TcpListener::bind(addr)
            .map_err(|e| Error::Config(format!("could not bind {address}: {e}")))?;
        let addr = tcp.local_addr()?;
        Ok(Self { tcp, addr, config })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Accept a TCP connection. The handshake is [`MtlsListener::secure`]'s
    /// job, on the connection's own thread, so one slow peer cannot stall
    /// the accept loop.
    pub fn accept(&self) -> io::Result<TcpStream> {
        self.tcp.accept().map(|(s, _)| s)
    }

    pub fn secure(&self, tcp: TcpStream) -> io::Result<ServerStream> {
        let _ = tcp.set_nodelay(true);
        tcp.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
        tcp.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
        let mut conn = ServerConnection::new(self.config.clone()).map_err(io::Error::other)?;
        let mut io = &tcp;
        while conn.is_handshaking() {
            match conn.complete_io(&mut io) {
                Ok((0, 0)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "peer left mid-handshake",
                    ));
                }
                Ok(_) => {}
                Err(e) => {
                    // Flush the alert so the client learns why.
                    let _ = conn.write_tls(&mut io);
                    return Err(e);
                }
            }
        }
        tcp.set_read_timeout(None)?;
        tcp.set_write_timeout(None)?;
        Ok(StreamOwned::new(conn, tcp))
    }

    /// Unblock a thread parked in [`MtlsListener::accept`].
    pub fn wake(&self) {
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_secs(1));
    }
}

#[path = "remote_dial.rs"]
mod dial;
pub use dial::{Dialer, Preferred};

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
