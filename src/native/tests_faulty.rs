//! A link to an in-process home with one data connection faulty (cut after
//! so many bytes, stalled, or slowed), and requests that can be checked or
//! failed on their way: the transfer tests' and the plan's failure tests'
//! way of making a unit's transfer go wrong.

use super::conn::Conn;
use super::frame::{Frame, Kind};
use super::session;
use crate::daemon::wire::{HomeReply, HomeRequest};
use crate::plan::{DataLink, HomeLink};
use crate::testutil::InProcess;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// What goes wrong, on which data connection (0-based, in opening order).
#[derive(Debug, Clone, Copy)]
pub enum Fault {
    /// Nothing: the data connections are as they are.
    None,
    /// Forward this many bytes (both ways together), then cut.
    CutAfter(usize, usize),
    /// Forward everything, slowly (a pause per 8 KiB each way).
    Throttle(usize, Duration),
    /// Home says Ready, then never answers anything.
    Stall(usize),
}

/// A check on each request, given how many of the unit's data connections
/// were opened so far: an error fails the request.
pub type OnCall<'a> = Box<dyn Fn(&HomeRequest, usize) -> crate::Result<()> + Sync + 'a>;

/// [`InProcess`], with one data connection faulty.
pub struct Faulty<'a> {
    pub inner: InProcess<'a>,
    pub fault: Fault,
    pub opened: AtomicUsize,
    /// Only this unit's data connections count, and fail.
    pub unit: Option<String>,
    pub on_call: Option<OnCall<'a>>,
    /// A stalled connection said hello; and later, the leaf let it go.
    pub stalled: std::sync::atomic::AtomicBool,
    pub released: std::sync::atomic::AtomicBool,
}

impl<'a> Faulty<'a> {
    pub fn new(inner: InProcess<'a>, fault: Fault) -> Self {
        Self {
            inner,
            fault,
            opened: AtomicUsize::new(0),
            unit: None,
            on_call: None,
            stalled: Default::default(),
            released: Default::default(),
        }
    }

    /// The fault only on `unit`'s data connections.
    pub fn at(mut self, unit: &str) -> Self {
        self.unit = Some(unit.into());
        self
    }

    pub fn on_call(mut self, f: OnCall<'a>) -> Self {
        self.on_call = Some(f);
        self
    }
}

impl HomeLink for Faulty<'_> {
    fn call(&self, r: HomeRequest) -> crate::Result<HomeReply> {
        if let Some(f) = &self.on_call {
            f(&r, self.opened.load(Ordering::SeqCst))?;
        }
        self.inner.call(r)
    }

    fn open_data(&self, rel: &str) -> crate::Result<DataLink<'_>> {
        if self.unit.as_deref().is_some_and(|u| u != rel) {
            return self.inner.open_data(rel);
        }
        let i = self.opened.fetch_add(1, Ordering::SeqCst);
        match self.fault {
            Fault::Stall(n) if n == i => {
                let (leaf, mut home) = Conn::pair()?;
                Ok(DataLink {
                    conn: leaf,
                    serve: Some(Box::new(move || {
                        if session::hello(&mut home).is_ok()
                            && home.send(&Frame::new(Kind::Ready, vec![])).is_ok()
                        {
                            self.stalled.store(true, Ordering::SeqCst);
                            let _ = home.set_timeout(Duration::from_secs(120));
                            let mut sink = [0u8; 65536];
                            while matches!(home.io().read(&mut sink), Ok(n) if n > 0) {}
                        }
                        self.released.store(true, Ordering::SeqCst);
                    })),
                })
            }
            Fault::CutAfter(n, _) | Fault::Throttle(n, _) if n == i => {
                let (limit, pause) = match self.fault {
                    Fault::CutAfter(_, l) => (l, Duration::ZERO),
                    Fault::Throttle(_, p) => (usize::MAX, p),
                    Fault::Stall(_) | Fault::None => unreachable!(),
                };
                use std::os::unix::net::UnixStream;
                let (leaf, from_leaf) = UnixStream::pair()?;
                let (to_home, home_end) = UnixStream::pair()?;
                let home = self.inner.home;
                Ok(DataLink {
                    conn: Conn::unix(leaf)?,
                    serve: Some(Box::new(move || {
                        // A cut kills both directions at once, as a dead
                        // network does, touching only the two sockets this
                        // fixture owns: each is the peer of a product socket.
                        // Shutting their write sides gives the leaf and home
                        // EOF on their next read; draining their read sides
                        // until EOF keeps a product write from ever blocking
                        // on a full buffer (the fixture no longer forwards,
                        // so nothing else would empty it; and a peer's
                        // shutdown does not wake a writer already blocked).
                        //
                        // Never the product's own socket: on macOS a read
                        // under SO_RCVTIMEO can hang for good when its socket
                        // is shut down from another thread, past its timeout
                        // (the race `daemon::local` works around with poll).
                        // Shutting home's end here did that, and hung the
                        // suite in one run of three or four.
                        let cut = |a: &UnixStream, b: &UnixStream| {
                            let _ = a.shutdown(std::net::Shutdown::Write);
                            let _ = b.shutdown(std::net::Shutdown::Write);
                        };
                        let drain = |r: &UnixStream| {
                            let mut sink = [0u8; 8192];
                            let mut r = r;
                            while matches!(r.read(&mut sink), Ok(n) if n > 0) {}
                        };
                        let moved = AtomicUsize::new(0);
                        // How much of `n` bytes may still pass.
                        let pass = |n: usize| {
                            let was = moved.fetch_add(n, Ordering::SeqCst);
                            n.min(limit.saturating_sub(was))
                        };
                        std::thread::scope(|s| {
                            s.spawn(|| {
                                let c = Conn::unix(home_end).unwrap();
                                home.serve_data(&crate::daemon::home::Caller::Trusted, c);
                            });
                            s.spawn(|| {
                                let (mut r, mut w) = (&from_leaf, &to_home);
                                let mut buf = [0u8; 8192];
                                while let Ok(n) = r.read(&mut buf) {
                                    let k = if n == 0 { 0 } else { pass(n) };
                                    std::thread::sleep(pause);
                                    if k < n {
                                        // The cut: both ways, at once.
                                        let _ = w.write_all(&buf[..k]);
                                        cut(&from_leaf, &to_home);
                                        return drain(&from_leaf);
                                    }
                                    if n == 0 || w.write_all(&buf[..n]).is_err() {
                                        break;
                                    }
                                }
                                // The leaf is done, or home is gone: pass the
                                // end on, and let home's last words through.
                                let _ = to_home.shutdown(std::net::Shutdown::Write);
                                drain(&from_leaf);
                            });
                            let (mut r, mut w) = (&to_home, &from_leaf);
                            let mut buf = [0u8; 8192];
                            while let Ok(n) = r.read(&mut buf) {
                                let k = if n == 0 { 0 } else { pass(n) };
                                std::thread::sleep(pause);
                                if k < n {
                                    let _ = w.write_all(&buf[..k]);
                                    break;
                                }
                                if n == 0 || w.write_all(&buf[..n]).is_err() {
                                    break;
                                }
                            }
                            cut(&from_leaf, &to_home);
                            drain(&to_home);
                        });
                    })),
                })
            }
            _ => self.inner.open_data(rel),
        }
    }
}
