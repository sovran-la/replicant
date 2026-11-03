//! A home in-process and a leaf in one tempdir, for transfers: the unit is
//! the loose `work` (root exclude `target`), data connections are socket
//! pairs, and [`Faulty`] can cut or stall one of them.

use super::conn::Conn;
use super::frame::{Frame, Kind};
use super::manifest::Manifest;
use super::session;
use super::transfer::{Counts, Diffed, Env, Ran, Transfer};
use crate::abort::Abort;
use crate::daemon::home::Home;
use crate::daemon::wire::{HomeReply, HomeRequest};
use crate::exclude::Excludes;
use crate::marker::Direction;
use crate::plan::{DataLink, HomeLink};
use crate::testutil::{Bed, InProcess, write};
use crate::units::{SubSpec, UnitKind};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

pub struct Fx {
    pub bed: Bed,
    pub home: Home,
}

pub fn excludes() -> Excludes {
    let spec = SubSpec {
        rel: "work".into(),
        exclude: vec!["target".into()],
        except: vec![],
        root: "work".into(),
    };
    crate::units::excludes(&spec, "work", UnitKind::Loose, &[], &[]).unwrap()
}

pub fn manifest(p: &Path) -> Manifest {
    Manifest::walk(p, &excludes(), false).unwrap()
}

/// Small files, a few large ones (several chunks each), links, nested and
/// empty directories, odd modes and nanosecond mtimes.
pub fn tree(base: &Path, big: usize) {
    use std::os::unix::fs::PermissionsExt;
    for i in 0..120 {
        write(
            &base.join(format!("small/{:03}/f{i}.txt", i % 7)),
            &format!("small {i}\n"),
        );
    }
    for i in 0..big {
        let bytes: Vec<u8> = (0..(1 << 20) * 2 + 12_345 * (i + 1))
            .map(|b| (b * 7 + i) as u8)
            .collect();
        std::fs::create_dir_all(base.join("big")).unwrap();
        std::fs::write(base.join(format!("big/blob{i}.bin")), bytes).unwrap();
    }
    write(&base.join("nested/a/b/c/deep.txt"), "deep");
    std::fs::create_dir_all(base.join("empty/dir")).unwrap();
    write(&base.join("exec.sh"), "#!/bin/sh\n");
    std::fs::set_permissions(base.join("exec.sh"), std::fs::Permissions::from_mode(0o751)).unwrap();
    std::fs::set_permissions(
        base.join("nested/a"),
        std::fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    std::os::unix::fs::symlink("nested/a/b", base.join("link-dir")).unwrap();
    std::os::unix::fs::symlink("/nonexistent/target", base.join("dangling")).unwrap();
    write(&base.join("target/junk"), "excluded: never moves");
    super::manifest::tests::set_mtime(&base.join("exec.sh"), 1_650_000_000, 123_456_789);
}

impl Fx {
    pub fn new() -> Self {
        let bed = Bed::enrolled();
        let home = bed.home();
        Self { bed, home }
    }

    pub fn link(&self) -> InProcess<'_> {
        InProcess {
            home: &self.home,
            host: "leaf".into(),
            home_dir: self.bed.leaf_hd.clone(),
        }
    }

    pub fn leaf(&self) -> PathBuf {
        self.bed.leaf_hd.join("work")
    }
    pub fn home_work(&self) -> PathBuf {
        self.bed.home_hd.join("work")
    }

    /// A sync's queue entry for `leaf`.
    pub fn lease(&self) -> u64 {
        match self
            .link()
            .call(HomeRequest::Acquire {
                no_wait: true,
                apply: true,
            })
            .unwrap()
        {
            HomeReply::Lease { lease, .. } => lease,
            other => panic!("{other:?}"),
        }
    }

    pub fn release(&self, lease: u64) {
        let _ = self.link().call(HomeRequest::Release { lease });
    }

    /// Home's marker and push run, as `BeginPush` makes them before a push.
    pub fn begin_push(&self, lease: u64) -> String {
        match self
            .link()
            .call(HomeRequest::BeginPush {
                lease,
                rel: "work".into(),
                source_state: crate::state::UnitState::Empty,
                backup: vec![],
                is_file: false,
            })
            .unwrap()
        {
            HomeReply::Begun { push } => push,
            other => panic!("{other:?}"),
        }
    }

    pub fn transfer(&self, direction: Direction) -> Transfer {
        Transfer {
            direction,
            rel: "work".into(),
            kind: UnitKind::Loose,
            leaf_path: self.leaf(),
            is_file: false,
            source_absent: false,
            excludes: excludes(),
            union: vec![],
            worktrees: vec![],
            ignored: vec![],
        }
    }
}

/// Run a dry run and its apply; the progress reported, in order.
pub fn run(
    link: &dyn HomeLink,
    t: &Transfer,
    lease: u64,
    streams: u32,
    abort: &Abort,
    ack: Duration,
) -> (Diffed, Ran, Vec<Counts>) {
    let seen: Mutex<Vec<Counts>> = Mutex::default();
    let progress = |c: Counts| seen.lock().unwrap().push(c);
    let env = Env {
        link,
        me: "leaf",
        lease,
        abort,
        streams,
        ack_timeout: ack,
        progress: &progress,
    };
    let d = t.dry_run(&env).unwrap();
    let ran = t.apply(&env, &d);
    (d, ran, seen.into_inner().unwrap())
}

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
                // A cut kills both ends, as a dead network does: home's own
                // socket too, or a write of home's would wait out its timeout.
                let home_ctl = Mutex::new(Some(home_end.try_clone()?));
                Ok(DataLink {
                    conn: Conn::unix(leaf)?,
                    serve: Some(Box::new(move || {
                        // Home's end first: on macOS a unix socket whose peer
                        // has shut down cannot be shut itself (ENOTCONN).
                        let cut = |a: &UnixStream, b: &UnixStream| {
                            if let Some(h) = home_ctl.lock().unwrap().as_ref() {
                                let _ = h.shutdown(std::net::Shutdown::Both);
                            }
                            let _ = a.shutdown(std::net::Shutdown::Both);
                            let _ = b.shutdown(std::net::Shutdown::Both);
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
                                // Home is done: its socket closes for real.
                                home_ctl.lock().unwrap().take();
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
                                        return cut(&from_leaf, &to_home);
                                    }
                                    if n == 0 || w.write_all(&buf[..n]).is_err() {
                                        break;
                                    }
                                }
                                // The leaf is done, or home is gone: pass the
                                // end on, and let home's last words through.
                                let _ = to_home.shutdown(std::net::Shutdown::Write);
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
                        });
                    })),
                })
            }
            _ => self.inner.open_data(rel),
        }
    }
}
