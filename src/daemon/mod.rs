//! The daemon: one per machine, home or leaf by the presence of fleet.toml.
//! PLAN.md, "Model: home base".
//!
//! | module | what it owns |
//! |---|---|
//! | [`certs`], [`tls`], [`remote`] | home's identity and the mTLS transport |
//! | [`wire`] | every request and reply that crosses a socket |
//! | [`home`] | home's handlers; [`fleet`] its `fleet_state` |
//! | [`leaf`] | a leaf's handlers and its link to home |
//! | [`queue`], [`journal`] | home's FIFO with leases; the append-only log |
//! | [`local`] | the CLI's Unix socket |
//!
//! Threads, not async: one accept loop per listener, one thread per
//! connection, one reaper for expired leases on home.

pub mod certs;
pub mod fleet;
pub mod home;
pub mod journal;
pub mod leaf;
pub mod local;
pub mod queue;
pub mod remote;
pub mod tls;
pub mod wire;

use crate::config::Dirs;
use crate::{Error, Result};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;
use wire::{LocalReply, LocalRequest};

const REAP_EVERY: Duration = Duration::from_millis(500);

/// A running daemon.
pub struct Daemon {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    dirs: Dirs,
    tcp: Option<Arc<remote::MtlsListener>>,
    /// Held for the daemon's life: the one-daemon-per-directory lock.
    _lock: File,
}

enum Role {
    Home(Arc<home::Home>),
    Leaf(Arc<leaf::Leaf>),
}

/// flock(2) the lock file, or say who holds it.
fn lock(dirs: &Dirs) -> Result<File> {
    std::fs::create_dir_all(&dirs.root)?;
    std::fs::set_permissions(&dirs.root, std::fs::Permissions::from_mode(0o700))?;
    let f = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dirs.lock())?;
    // SAFETY: the fd is valid for the life of `f`.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::Daemon(format!(
            "a replicant daemon is already running for {}",
            dirs.root.display()
        )));
    }
    Ok(f)
}

impl Daemon {
    /// Claim the directory, bind everything, and start serving.
    pub fn start(dirs: Dirs) -> Result<Self> {
        let lock = lock(&dirs)?;
        let role = if dirs.is_home() {
            certs::ensure(&dirs.ca_key(), &dirs.ca_cert(), &dirs.credentials())?;
            Role::Home(Arc::new(home::Home::open(dirs.clone())?))
        } else {
            Role::Leaf(Arc::new(leaf::Leaf::open(dirs.clone())))
        };
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();

        let tcp = match &role {
            Role::Home(h) => {
                let f = crate::config::Fleet::load(&dirs.fleet())?;
                let listen = f.listen.ok_or_else(|| {
                    Error::Config(
                        "home needs [daemon] listen = \"<tailnet address>:<port>\" in fleet.toml"
                            .into(),
                    )
                })?;
                let identity = tls::Identity::load(&dirs.credentials())?;
                let l = Arc::new(remote::MtlsListener::bind(
                    &crate::config::with_port(&listen),
                    &identity,
                )?);
                threads.push(spawn_tcp(l.clone(), h.clone(), stop.clone()));
                threads.push(spawn_reaper(h.clone(), stop.clone()));
                Some(l)
            }
            Role::Leaf(_) => None,
        };

        // The lock is held, so any socket file here is a dead daemon's.
        let _ = std::fs::remove_file(dirs.socket());
        let unix = UnixListener::bind(dirs.socket())?;
        std::fs::set_permissions(dirs.socket(), std::fs::Permissions::from_mode(0o600))?;
        let role = Arc::new(role);
        let s = stop.clone();
        threads.push(std::thread::spawn(move || {
            for conn in unix.incoming() {
                if s.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(conn) = conn else { continue };
                let role = role.clone();
                std::thread::spawn(move || local::serve(conn, &|req| handle_local(&role, req)));
            }
        }));
        Ok(Self {
            stop,
            threads,
            dirs,
            tcp,
            _lock: lock,
        })
    }

    /// Where home's mTLS listener is bound (home only).
    pub fn tcp_addr(&self) -> Option<std::net::SocketAddr> {
        self.tcp.as_ref().map(|l| l.local_addr())
    }

    /// Block until the daemon stops (it does not, short of a signal).
    pub fn wait(self) {
        for t in self.threads {
            let _ = t.join();
        }
    }

    /// Stop accepting, wake every listener, and wait for the loops to exit.
    pub fn stop(self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(self.dirs.socket());
        if let Some(l) = &self.tcp {
            l.wake();
        }
        for t in self.threads {
            let _ = t.join();
        }
        let _ = std::fs::remove_file(self.dirs.socket());
    }
}

fn spawn_tcp(
    l: Arc<remote::MtlsListener>,
    home: Arc<home::Home>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        loop {
            let conn = l.accept();
            if stop.load(Ordering::SeqCst) {
                break;
            }
            let Ok(tcp) = conn else { continue };
            let (l, home) = (l.clone(), home.clone());
            std::thread::spawn(move || {
                let mut s = match l.secure(tcp) {
                    Ok(s) => s,
                    Err(e) => return eprintln!("replicant: refused a connection: {e}"),
                };
                let answer = match remote::recv::<_, wire::HomeCall>(&mut s) {
                    Ok(call) => home.handle(call),
                    Err(e) => wire::HomeAnswer {
                        result: Err(e.to_string()),
                        journal: vec![],
                    },
                };
                let _ = remote::send(&mut s, &answer);
                s.conn.send_close_notify();
                let _ = s.conn.complete_io(&mut s.sock);
            });
        }
    })
}

fn spawn_reaper(home: Arc<home::Home>, stop: Arc<AtomicBool>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            home.reap();
            std::thread::sleep(REAP_EVERY);
        }
    })
}

fn handle_local(role: &Role, req: LocalRequest) -> wire::LocalAnswer {
    match role {
        Role::Leaf(l) => l.handle(req),
        Role::Home(h) => home_local(h, req).map_err(|e| e.to_string()),
    }
}

fn home_local(h: &home::Home, req: LocalRequest) -> Result<LocalReply> {
    match req {
        LocalRequest::Ping => Ok(LocalReply::Pong {
            role: "home".into(),
            name: h.name()?,
        }),
        LocalRequest::Status | LocalRequest::Fleet => Ok(LocalReply::Fleet(h.fleet_state()?)),
        LocalRequest::Log { filter } => Ok(LocalReply::Log {
            lines: h.log(&filter)?,
        }),
        LocalRequest::Pending => Ok(LocalReply::Pending {
            hosts: h.pending()?,
        }),
        LocalRequest::PreviewAllow { host, raw } => {
            Ok(LocalReply::AllowPreview(h.preview_allow(&host, &raw)?))
        }
        LocalRequest::Approve { host, allow } => {
            Ok(LocalReply::Approved(h.approve(&host, &allow)?))
        }
        LocalRequest::Subscribe { .. }
        | LocalRequest::Unsubscribe { .. }
        | LocalRequest::DryRun { .. } => Err(Error::Refused(
            "this machine is home; subscribe and sync run on a leaf".into(),
        )),
    }
}

#[cfg(test)]
#[path = "daemon_tests.rs"]
mod tests;
