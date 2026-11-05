//! A socket whose reads can be ended from another thread, reliably.
//!
//! On macOS a read blocked under `SO_RCVTIMEO` can miss the wakeup when its
//! own socket is shut down from another thread, and sleep for good (the A21
//! hang in `docs/v0.4.2-addresses-review.md`: 5 wedged reads in 160,000
//! `AF_UNIX` rounds beside load; TCP never showed it in 48,000, which is too
//! few to clear it). A read that `poll(2)`s first, on the socket and on a
//! pipe of its own, has a wakeup nothing can lose: the thread that wants it
//! to stop writes the pipe. (`daemon::local` ends the CLI watcher through a
//! pipe too, but by closing its write end, which never raises SIGPIPE; this
//! one writes, so its read end must outlive every writer: see [`Stop`].)
//!
//! [`Polled`] is the transport under a connection (a TLS stream reads and
//! writes through it; an in-process socket pair is wrapped directly). Its
//! [`Stop`] is the handle the transfer monitor keeps: `stop` wakes the pipe
//! and shuts the socket down too, for the peer's sake and for a write that
//! may be blocked. Writes are not polled: a blocked write ends on the
//! socket's own write timeout, as before.

use std::io::{self, PipeReader, PipeWriter, Read, Write};
use std::net::TcpStream;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// No read timeout: wait for data, or the stop.
const FOREVER: u64 = 0;

pub struct Polled<S> {
    inner: S,
    /// Shared with every [`Stop`], so the read end outlives the reader: a
    /// stop raised after the connection is gone must find a pipe with a
    /// reader, or its write is EPIPE, and that is SIGPIPE to a process with
    /// the default disposition. 0.4.6 as tagged killed a leaf daemon that
    /// way on any failed transfer (review A41).
    woken: Arc<PipeReader>,
    wake: PipeWriter,
    /// The read timeout in milliseconds, [`FOREVER`] for none; shared with
    /// every [`Stop`] so [`Stop::set_timeout`] reaches the poll.
    timeout_ms: Arc<AtomicU64>,
}

/// The socket under a [`Polled`].
pub enum Raw {
    Tcp(TcpStream),
    Unix(UnixStream),
}

/// Another thread's handle on a [`Polled`]: its timeouts, and the stop.
pub struct Stop {
    sock: Raw,
    wake: PipeWriter,
    /// Keeps the pipe's read end open for as long as this handle lives.
    _woken: Arc<PipeReader>,
    timeout_ms: Arc<AtomicU64>,
}

impl<S: AsRawFd> Polled<S> {
    fn wrap(inner: S) -> io::Result<Self> {
        let (woken, wake) = io::pipe()?;
        // A stop that is raised many times must never block on a full pipe.
        // SAFETY: a valid descriptor this value owns.
        unsafe {
            let flags = libc::fcntl(wake.as_raw_fd(), libc::F_GETFL);
            if flags >= 0 {
                libc::fcntl(wake.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }
        Ok(Self {
            inner,
            woken: Arc::new(woken),
            wake,
            timeout_ms: Arc::new(AtomicU64::new(FOREVER)),
        })
    }

    pub fn inner(&self) -> &S {
        &self.inner
    }

    fn stop_with(&self, sock: Raw) -> io::Result<Stop> {
        Ok(Stop {
            sock,
            wake: self.wake.try_clone()?,
            _woken: self.woken.clone(),
            timeout_ms: self.timeout_ms.clone(),
        })
    }

    /// Wait until the socket is readable, the stop is raised, or the
    /// timeout passes. `Ok(())` means a read will not block.
    fn wait(&self) -> io::Result<()> {
        loop {
            let mut fds = [
                libc::pollfd {
                    fd: self.inner.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.woken.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let ms = match self.timeout_ms.load(Ordering::SeqCst) {
                FOREVER => -1,
                ms => ms.min(i32::MAX as u64) as i32,
            };
            // SAFETY: two live pollfds for descriptors this value owns.
            let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, ms) };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "no data within the timeout",
                ));
            }
            if fds[1].revents != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "the connection was stopped",
                ));
            }
            // Readable, hung up, or in error: the read returns at once.
            return Ok(());
        }
    }
}

impl Polled<TcpStream> {
    /// Wrap a TCP socket; its stop handle holds a clone of it.
    pub fn tcp(s: TcpStream) -> io::Result<(Self, Stop)> {
        let p = Self::wrap(s)?;
        let stop = p.stop_with(Raw::Tcp(p.inner.try_clone()?))?;
        Ok((p, stop))
    }

    /// One more stop handle for a TCP socket already wrapped.
    pub fn stop(&self) -> io::Result<Stop> {
        self.stop_with(Raw::Tcp(self.inner.try_clone()?))
    }

    pub fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.timeout_ms
            .store(d.map_or(FOREVER, to_ms), Ordering::SeqCst);
        self.inner.set_read_timeout(d)
    }

    pub fn set_write_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.inner.set_write_timeout(d)
    }

    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        self.inner.read_timeout()
    }

    pub fn write_timeout(&self) -> io::Result<Option<Duration>> {
        self.inner.write_timeout()
    }
}

impl Polled<UnixStream> {
    /// Wrap an in-process socket; its stop handle holds a clone of it.
    pub fn unix(s: UnixStream) -> io::Result<(Self, Stop)> {
        let p = Self::wrap(s)?;
        let stop = p.stop_with(Raw::Unix(p.inner.try_clone()?))?;
        Ok((p, stop))
    }
}

impl<S: AsRawFd + Read> Read for Polled<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.wait()?;
        self.inner.read(buf)
    }
}

impl<S: Write> Write for Polled<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// A timeout of under a millisecond still polls, rather than waiting forever.
fn to_ms(d: Duration) -> u64 {
    (d.as_millis() as u64).max(1)
}

impl Stop {
    pub fn try_clone(&self) -> io::Result<Stop> {
        Ok(Stop {
            sock: match &self.sock {
                Raw::Tcp(s) => Raw::Tcp(s.try_clone()?),
                Raw::Unix(s) => Raw::Unix(s.try_clone()?),
            },
            wake: self.wake.try_clone()?,
            _woken: self._woken.clone(),
            timeout_ms: self.timeout_ms.clone(),
        })
    }

    /// Read and write timeouts both: the poll's, and the socket's own.
    pub fn set_timeout(&self, d: Duration) -> io::Result<()> {
        self.timeout_ms.store(to_ms(d), Ordering::SeqCst);
        match &self.sock {
            Raw::Tcp(s) => {
                s.set_read_timeout(Some(d))?;
                s.set_write_timeout(Some(d))
            }
            Raw::Unix(s) => {
                s.set_read_timeout(Some(d))?;
                s.set_write_timeout(Some(d))
            }
        }
    }

    /// Stop the connection: a read blocked on it returns now (the pipe), the
    /// peer sees it end and a blocked write fails (the shutdown).
    pub fn stop(&self) {
        let _ = (&self.wake).write(&[1]);
        let _ = match &self.sock {
            Raw::Tcp(s) => s.shutdown(std::net::Shutdown::Both),
            Raw::Unix(s) => s.shutdown(std::net::Shutdown::Both),
        };
    }
}

#[cfg(test)]
#[path = "polled_tests.rs"]
mod tests;
