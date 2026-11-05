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
                // The fixture's own two sockets, each the peer of a product
                // socket, are read and written in short slices so a stop can
                // be noticed between them. Nothing is ever shut down: a cut
                // stops both forwarders and then the sockets are *closed*,
                // which gives the leaf and home EOF and EPIPE at once.
                //
                // Why only a close: on macOS AF_UNIX a read under SO_RCVTIMEO
                // can wedge for good when its own socket is shut down from
                // another thread (what 0.4.4's cut did to home's end, and what
                // hung the suite in one run of three or four), and can miss
                // the wakeup of a peer's shutdown(Write) and return only at its
                // timeout (0.4.5's cut: the 20 s ack timeout in a few runs per
                // thousand under load). In 400k rounds a peer's close never
                // lost its wakeup. See docs/v0.4.2-addresses-review.md, A21
                // and A30.
                const SLICE: Duration = Duration::from_millis(10);
                for s in [&from_leaf, &to_home] {
                    s.set_read_timeout(Some(SLICE))?;
                    s.set_write_timeout(Some(SLICE))?;
                }
                Ok(DataLink {
                    conn: Conn::unix(leaf)?,
                    serve: Some(Box::new(move || {
                        use std::io::ErrorKind::{Interrupted, TimedOut, WouldBlock};
                        let stop = std::sync::atomic::AtomicBool::new(false);
                        let moved = AtomicUsize::new(0);
                        // How much of `n` bytes may still pass.
                        let pass = |n: usize| {
                            let was = moved.fetch_add(n, Ordering::SeqCst);
                            n.min(limit.saturating_sub(was))
                        };
                        // Forward `r` to `w` until the end, the limit, or a
                        // stop. `to_home`: a write home refuses means home is
                        // gone, which is not a cut: the other way keeps
                        // flowing so home's last words reach the leaf.
                        let forward = |mut r: &UnixStream, mut w: &UnixStream, to_home: bool| {
                            let mut buf = [0u8; 8192];
                            'run: while !stop.load(Ordering::SeqCst) {
                                let n = match r.read(&mut buf) {
                                    Ok(0) => break,
                                    Ok(n) => n,
                                    Err(e)
                                        if matches!(
                                            e.kind(),
                                            WouldBlock | TimedOut | Interrupted
                                        ) =>
                                    {
                                        continue;
                                    }
                                    Err(_) => break,
                                };
                                let k = pass(n);
                                std::thread::sleep(pause);
                                let mut left = &buf[..k];
                                while !left.is_empty() {
                                    if stop.load(Ordering::SeqCst) {
                                        break 'run;
                                    }
                                    match w.write(left) {
                                        Ok(m) => left = &left[m..],
                                        Err(e)
                                            if matches!(
                                                e.kind(),
                                                WouldBlock | TimedOut | Interrupted
                                            ) => {}
                                        Err(_) if to_home => return,
                                        Err(_) => break 'run,
                                    }
                                }
                                if k < n {
                                    // The cut: both ways, at once.
                                    break;
                                }
                            }
                            stop.store(true, Ordering::SeqCst);
                        };
                        std::thread::scope(|s| {
                            s.spawn(|| {
                                let c = Conn::unix(home_end).unwrap();
                                home.serve_data(&crate::daemon::home::Caller::Trusted, c);
                            });
                            let (a, b) = (from_leaf, to_home);
                            std::thread::scope(|f| {
                                f.spawn(|| forward(&a, &b, true));
                                forward(&b, &a, false);
                            });
                            // A real close: the only wakeup never lost.
                            drop(a);
                            drop(b);
                        });
                    })),
                })
            }
            _ => self.inner.open_data(rel),
        }
    }
}
