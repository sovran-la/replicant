//! One data connection: a byte stream (mTLS between daemons, a socket pair
//! in-process) and the socket under it, kept so another thread can stop it.
//!
//! The socket's read and write timeouts are the ack timeout: a frame that
//! gets no answer, or a write the peer stops draining, within it fails with
//! [`super::frame::io_error`]'s "connection is dead". Shutting the socket
//! down from another thread (an abort) fails whatever is blocked on it at
//! once.

use super::frame::{self, Frame};
use crate::Result;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// A stream frames can be read from and written to.
pub trait Duplex: Read + Write + Send {}
impl<T: Read + Write + Send> Duplex for T {}

/// The socket under a stream.
#[derive(Debug)]
pub enum Sock {
    Tcp(TcpStream),
    Unix(UnixStream),
}

impl Sock {
    pub fn try_clone(&self) -> io::Result<Sock> {
        Ok(match self {
            Sock::Tcp(s) => Sock::Tcp(s.try_clone()?),
            Sock::Unix(s) => Sock::Unix(s.try_clone()?),
        })
    }

    /// Read and write timeouts both.
    pub fn set_timeout(&self, d: Duration) -> io::Result<()> {
        match self {
            Sock::Tcp(s) => {
                s.set_read_timeout(Some(d))?;
                s.set_write_timeout(Some(d))
            }
            Sock::Unix(s) => {
                s.set_read_timeout(Some(d))?;
                s.set_write_timeout(Some(d))
            }
        }
    }

    /// Stop the connection: whatever is blocked on it fails now.
    pub fn shutdown(&self) {
        let _ = match self {
            Sock::Tcp(s) => s.shutdown(Shutdown::Both),
            Sock::Unix(s) => s.shutdown(Shutdown::Both),
        };
    }
}

pub struct Conn {
    io: Box<dyn Duplex>,
    sock: Sock,
}

impl Conn {
    pub fn new(io: Box<dyn Duplex>, sock: Sock) -> Self {
        Self { io, sock }
    }

    /// A plain socket as a connection (in-process, and the tests).
    pub fn unix(s: UnixStream) -> io::Result<Self> {
        let sock = Sock::Unix(s.try_clone()?);
        Ok(Self::new(Box::new(s), sock))
    }

    /// Two ends of one in-process connection.
    pub fn pair() -> io::Result<(Self, Self)> {
        let (a, b) = UnixStream::pair()?;
        Ok((Self::unix(a)?, Self::unix(b)?))
    }

    pub fn send(&mut self, f: &Frame) -> Result<()> {
        frame::write(&mut self.io, f).map_err(frame::io_error)
    }

    pub fn recv(&mut self) -> Result<Frame> {
        frame::read(&mut self.io)
    }

    pub fn set_timeout(&self, d: Duration) -> Result<()> {
        Ok(self.sock.set_timeout(d)?)
    }

    /// A handle another thread can [`Sock::shutdown`] this connection with.
    pub fn closer(&self) -> Result<Sock> {
        Ok(self.sock.try_clone()?)
    }

    /// The raw stream: the bytes before the first frame ([`frame::MAGIC`]).
    pub fn io(&mut self) -> &mut dyn Duplex {
        &mut *self.io
    }
}
