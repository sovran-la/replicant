//! Dialing out to home: which address, in what order, and what to say when
//! none answers.
//!
//! Home may be listed at several addresses (the LAN name, then the tailnet
//! address). A dialer tries them in order and then sticks with the one that
//! answered, down to the IP it resolved to, so a request's many connections
//! pay for the dead candidates once. Clones share that choice. Every
//! connection still runs the full handshake, so the choice is routing, never
//! trust.

use super::HANDSHAKE_TIMEOUT;
use crate::daemon::certs::SERVER_NAME;
use crate::daemon::tls::Identity;
use crate::native::conn::{Conn, Sock};
use crate::{Error, Result};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, StreamOwned};
use serde::{Serialize, de::DeserializeOwned};
use std::io::Write;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub type Stream = StreamOwned<ClientConnection, TcpStream>;

/// The listed address that answered last, and the IP it answered at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preferred {
    pub entry: String,
    pub addr: SocketAddr,
}

/// Why one listed address did not yield a connection.
#[derive(Debug)]
enum Miss {
    /// No TCP connection, or the name did not resolve.
    Unreachable(String),
    /// Something answered and the TLS handshake failed: not home, or not
    /// this bundle's home.
    Refused(String),
}

#[derive(Clone)]
pub struct Dialer {
    config: Arc<ClientConfig>,
    connect_timeout: Duration,
    /// Bound on the TLS handshake, separate from the request deadline: a
    /// peer that accepts TCP and then says nothing must not hold a dial for
    /// the 900 s a request may take.
    handshake_timeout: Duration,
    source: String,
    preferred: Arc<Mutex<Option<Preferred>>>,
}

impl Dialer {
    pub fn new(identity: &Identity, connect_timeout: Duration) -> Result<Self> {
        Ok(Self {
            config: identity.client_config()?,
            connect_timeout,
            handshake_timeout: HANDSHAKE_TIMEOUT,
            source: identity.source.display().to_string(),
            preferred: Arc::new(Mutex::new(None)),
        })
    }

    #[cfg(test)]
    pub fn with_handshake_timeout(mut self, t: Duration) -> Self {
        self.handshake_timeout = t;
        self
    }

    /// One request, one reply, then the connection closes.
    pub fn call<Q: Serialize, A: DeserializeOwned>(
        &self,
        address: &str,
        req: &Q,
        deadline: Duration,
    ) -> Result<A> {
        self.call_any(std::slice::from_ref(&address.to_string()), req, deadline)
    }

    /// [`Dialer::call`] over the first of `addresses` that answers.
    pub fn call_any<Q: Serialize, A: DeserializeOwned>(
        &self,
        addresses: &[String],
        req: &Q,
        deadline: Duration,
    ) -> Result<A> {
        let mut stream = self.connect_any(addresses, deadline)?;
        super::send(&mut stream, req)?;
        super::recv(&mut stream)
    }

    /// A data connection to home: the same handshake as a request, the
    /// magic line, then frames (`native::session::open` says hello on it).
    pub fn open_data(&self, address: &str, timeout: Duration) -> Result<Conn> {
        self.open_data_any(std::slice::from_ref(&address.to_string()), timeout)
    }

    /// [`Dialer::open_data`] over the first of `addresses` that answers.
    pub fn open_data_any(&self, addresses: &[String], timeout: Duration) -> Result<Conn> {
        let mut stream = self.connect_any(addresses, timeout)?;
        stream.write_all(crate::native::frame::MAGIC)?;
        let sock = Sock::Tcp(stream.sock.try_clone()?);
        Ok(Conn::new(Box::new(stream), sock))
    }

    /// What answered last, if anything has.
    pub fn preferred(&self) -> Option<Preferred> {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Preferred>> {
        self.preferred.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A connection over one address, nothing sent on it.
    #[cfg(test)]
    pub(in crate::daemon) fn connect(&self, address: &str, deadline: Duration) -> Result<Stream> {
        self.connect_any(std::slice::from_ref(&address.to_string()), deadline)
    }

    /// `addresses` with the preferred entry (if it is still listed) first,
    /// the rest in their order; and the IP to try first for that entry.
    pub(super) fn order<'a>(
        &self,
        addresses: &'a [String],
    ) -> (Vec<&'a String>, Option<Preferred>) {
        let preferred = self.preferred().filter(|p| addresses.contains(&p.entry));
        let mut out: Vec<&String> = Vec::with_capacity(addresses.len());
        if let Some(a) = preferred
            .as_ref()
            .and_then(|p| addresses.iter().find(|a| **a == p.entry))
        {
            out.push(a);
        }
        out.extend(
            addresses
                .iter()
                .filter(|a| preferred.as_ref().is_none_or(|p| **a != p.entry)),
        );
        (out, preferred)
    }

    /// A connection to home over the first of `addresses` that completes
    /// the handshake, preferred first. Nothing has been sent on it.
    ///
    /// A single address keeps its own error. With several, every attempt is
    /// reported, one per line, and the error is a handshake refusal only if
    /// every attempt was one. An address that was skipped on the way to one
    /// that answered is logged: a sync quietly running over the slow path, or
    /// something at the LAN address that is not home, is worth a line.
    pub(in crate::daemon) fn connect_any(
        &self,
        addresses: &[String],
        deadline: Duration,
    ) -> Result<Stream> {
        let (order, preferred) = self.order(addresses);
        let was = preferred.clone();
        let mut misses: Vec<(&String, Miss)> = Vec::new();
        for entry in &order {
            let first = preferred
                .as_ref()
                .filter(|p| p.entry == **entry)
                .map(|p| p.addr);
            match self.connect_entry(entry, first, deadline) {
                Ok((stream, addr)) => {
                    *self.lock() = Some(Preferred {
                        entry: (*entry).clone(),
                        addr,
                    });
                    if !misses.is_empty() {
                        eprintln!(
                            "replicant: home answered at {entry}; skipped {}",
                            misses
                                .iter()
                                .map(|(a, m)| format!("{a} ({})", m.detail()))
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                    }
                    return Ok(stream);
                }
                Err(miss) => misses.push((entry, miss)),
            }
        }
        self.forget(&was);
        Err(match misses.as_slice() {
            [] => Error::Remote("home has no address to dial".into()),
            [(entry, miss)] => miss.single(entry, &self.source),
            many => {
                let lines: Vec<String> = many
                    .iter()
                    .map(|(a, m)| format!("  {a}: {}", m.detail()))
                    .collect();
                let text = format!("home did not answer at any address:\n{}", lines.join("\n"));
                if many.iter().all(|(_, m)| matches!(m, Miss::Refused(_))) {
                    Error::Tls(format!(
                        "{text}\nnone accepted the handshake with {}. Is that home's current \
                         bundle?",
                        self.source
                    ))
                } else {
                    Error::Remote(text)
                }
            }
        })
    }

    /// Forget the choice, but only if it is still `was`: a dial that failed
    /// must not erase a newer choice another thread made meanwhile.
    pub(super) fn forget(&self, was: &Option<Preferred>) {
        let mut p = self.lock();
        if *p == *was {
            *p = None;
        }
    }

    #[cfg(test)]
    pub(super) fn handshake_timeout(&self) -> Duration {
        self.handshake_timeout
    }

    /// One listed address: resolve it, try `first` before the rest of what
    /// it resolved to, and complete the handshake.
    fn connect_entry(
        &self,
        address: &str,
        first: Option<SocketAddr>,
        deadline: Duration,
    ) -> std::result::Result<(Stream, SocketAddr), Miss> {
        let addrs: Vec<SocketAddr> = address
            .to_socket_addrs()
            .map_err(|e| Miss::Unreachable(format!("does not resolve ({e})")))?
            .collect();
        let resolved_any = !addrs.is_empty();
        let addrs = candidates(addrs, first);
        let mut last = if resolved_any && addrs.is_empty() {
            "resolves only to link-local IPv6 addresses without a scope".to_string()
        } else {
            "resolves to no address".to_string()
        };
        let mut tcp = None;
        for a in addrs {
            match TcpStream::connect_timeout(&a, self.connect_timeout) {
                Ok(s) => {
                    tcp = Some((s, a));
                    break;
                }
                Err(e) => last = e.to_string(),
            }
        }
        let (tcp, addr) = tcp.ok_or(Miss::Unreachable(last))?;
        let io = |e: std::io::Error| Miss::Unreachable(e.to_string());
        let hs = deadline.min(self.handshake_timeout);
        tcp.set_read_timeout(Some(hs)).map_err(io)?;
        tcp.set_write_timeout(Some(hs)).map_err(io)?;
        let _ = tcp.set_nodelay(true);
        let name = ServerName::try_from(SERVER_NAME).map_err(|e| Miss::Refused(e.to_string()))?;
        let conn = ClientConnection::new(self.config.clone(), name)
            .map_err(|e| Miss::Refused(e.to_string()))?;
        let mut stream = StreamOwned::new(conn, tcp);
        // A peer that accepts TCP and then says nothing, or goes away mid
        // handshake, is unreachable, not a refusal: no certificate was judged.
        stream.conn.complete_io(&mut stream.sock).map_err(|e| {
            use std::io::ErrorKind::*;
            match e.kind() {
                WouldBlock | TimedOut => {
                    Miss::Unreachable(format!("no answer to the handshake within {:?}", hs))
                }
                UnexpectedEof | ConnectionReset | ConnectionAborted | BrokenPipe => {
                    Miss::Unreachable(format!("closed during the handshake ({e})"))
                }
                _ => Miss::Refused(e.to_string()),
            }
        })?;
        stream.sock.set_read_timeout(Some(deadline)).map_err(io)?;
        stream.sock.set_write_timeout(Some(deadline)).map_err(io)?;
        Ok((stream, addr))
    }
}

impl Miss {
    fn detail(&self) -> &str {
        match self {
            Miss::Unreachable(d) => d,
            Miss::Refused(d) => d,
        }
    }

    /// The error for a dial that had only this address to try.
    fn single(&self, address: &str, source: &str) -> Error {
        match self {
            Miss::Unreachable(d) => Error::Remote(format!("home at {address} is unreachable: {d}")),
            Miss::Refused(d) => Error::Tls(format!(
                "home at {address} refused the handshake with {source} ({d}). Is that home's \
                 current bundle?"
            )),
        }
    }
}

/// The order to try a name's addresses in: IPv4 before IPv6, and never an
/// IPv6 link-local address without a scope. The resolver hands those back
/// for mDNS names (`host.local`), but without a scope the kernel cannot pick
/// an interface, so the SYN goes nowhere and the dial sits out the whole
/// connect timeout before the IPv4 address even gets a turn.
pub(super) fn dial_order(addrs: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let mut v4 = Vec::new();
    let mut v6 = Vec::new();
    for a in addrs {
        match a {
            SocketAddr::V4(_) => v4.push(a),
            SocketAddr::V6(six) if is_link_local(six.ip()) && six.scope_id() == 0 => {}
            SocketAddr::V6(_) => v6.push(a),
        }
    }
    v4.extend(v6);
    v4
}

/// fe80::/10.
fn is_link_local(ip: &std::net::Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xffc0) == 0xfe80
}

/// What a name resolved to, in dial order, with `first` (the IP that
/// answered last time, if it is still among them) moved to the front.
pub(super) fn candidates(resolved: Vec<SocketAddr>, first: Option<SocketAddr>) -> Vec<SocketAddr> {
    let mut addrs = dial_order(resolved);
    if let Some(f) = first.filter(|f| addrs.contains(f)) {
        addrs.retain(|a| *a != f);
        addrs.insert(0, f);
    }
    addrs
}
