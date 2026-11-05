//! One data connection: a byte stream (mTLS between daemons, a socket pair
//! in-process) over a [`Polled`] socket, and the [`Stop`] another thread
//! ends it with.
//!
//! The read and write timeouts are the ack timeout: a frame that gets no
//! answer, or a write the peer stops draining, within it fails with
//! [`super::frame::io_error`]'s "connection is dead". Stopping the connection
//! from another thread (an abort) ends a blocked read at once, through the
//! polled socket's own pipe; see [`super::polled`] for why a plain shutdown
//! is not enough on macOS.

use super::frame::{self, Frame};
use super::polled::{Polled, Stop};
use crate::Result;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// A stream frames can be read from and written to.
pub trait Duplex: Read + Write + Send {}
impl<T: Read + Write + Send> Duplex for T {}

pub struct Conn {
    io: Box<dyn Duplex>,
    stop: Stop,
}

impl Conn {
    pub fn new(io: Box<dyn Duplex>, stop: Stop) -> Self {
        Self { io, stop }
    }

    /// A plain socket as a connection (in-process, and the tests).
    pub fn unix(s: UnixStream) -> io::Result<Self> {
        let (polled, stop) = Polled::unix(s)?;
        Ok(Self::new(Box::new(polled), stop))
    }

    /// Two ends of one in-process connection.
    pub fn pair() -> io::Result<(Self, Self)> {
        let (a, b) = UnixStream::pair()?;
        Ok((Self::unix(a)?, Self::unix(b)?))
    }

    /// Send one frame. If the peer has already hung up, what it said
    /// before it did (a [`frame::Kind::Fail`] or `Refused` still unread
    /// behind any acks) is the error, rather than a broken pipe.
    pub fn send(&mut self, f: &Frame) -> Result<()> {
        match frame::write(&mut self.io, f) {
            Ok(()) => Ok(()),
            // Never written: the peer has nothing to say about it.
            Err(e) if e.kind() == io::ErrorKind::InvalidInput => Err(frame::io_error(e)),
            Err(e) => Err(self.last_word().unwrap_or_else(|| frame::io_error(e))),
        }
    }

    /// The reason a departed peer left in the stream, if any.
    fn last_word(&mut self) -> Option<crate::Error> {
        let _ = self.stop.set_timeout(Duration::from_secs(1));
        for _ in 0..64 {
            match frame::read(&mut self.io) {
                Ok(f) if matches!(f.kind, frame::Kind::Fail | frame::Kind::Refused) => {
                    return f.expect(frame::Kind::Ack).err();
                }
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
        None
    }

    pub fn recv(&mut self) -> Result<Frame> {
        frame::read(&mut self.io)
    }

    pub fn set_timeout(&self, d: Duration) -> Result<()> {
        Ok(self.stop.set_timeout(d)?)
    }

    /// A handle another thread can [`Stop::stop`] this connection with.
    pub fn stopper(&self) -> Result<Stop> {
        Ok(self.stop.try_clone()?)
    }

    /// The raw stream: the bytes before the first frame ([`frame::MAGIC`]).
    pub fn io(&mut self) -> &mut dyn Duplex {
        &mut *self.io
    }
}
