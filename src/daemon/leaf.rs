//! A leaf's daemon: the CLI's requests, relayed to home over mTLS where they
//! need home, and the leaf's own copy of its journal lines.

use super::journal::Journal;
use super::remote::Dialer;
use super::tls::Identity;
use super::wire::*;
use crate::config::{Dirs, HomeEntry, LeafConfig, leaf::check_local, short_hostname, with_port};
use crate::plan::{self, HomeLink, LeafCtx, LeafStatus};
use crate::{Error, Result, paths};
use std::path::PathBuf;
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Long enough to wait out the queue and hash home's side of a large tree.
const REQUEST_DEADLINE: Duration = Duration::from_secs(900);
/// Heartbeats and releases: short, so a hung call cannot outlast the lease
/// it is meant to keep, and the heartbeat thread gets to retry.
const LEASE_CALL_DEADLINE: Duration = Duration::from_secs(10);

pub struct Leaf {
    pub dirs: Dirs,
    pub journal: Journal,
}

/// The mTLS link to home, as this leaf.
struct Remote<'a> {
    journal: &'a Journal,
    address: String,
    me: String,
    home_dir: PathBuf,
    dialer: Dialer,
}

impl HomeLink for Remote<'_> {
    fn call(&self, request: HomeRequest) -> Result<HomeReply> {
        let deadline = match request {
            HomeRequest::Heartbeat { .. } | HomeRequest::Release { .. } => LEASE_CALL_DEADLINE,
            _ => REQUEST_DEADLINE,
        };
        let call = HomeCall {
            version: WIRE_VERSION,
            host: self.me.clone(),
            home_dir: self.home_dir.clone(),
            request,
        };
        let answer: HomeAnswer = self.dialer.call(&self.address, &call, deadline)?;
        for line in &answer.journal {
            if let Err(e) = self.journal.append_raw(line) {
                eprintln!("replicant: could not keep a local copy of a journal line: {e}");
            }
        }
        answer.result.map_err(Error::Remote)
    }
}

impl Leaf {
    pub fn open(dirs: Dirs) -> Self {
        Self {
            journal: Journal::at(dirs.journal()),
            dirs,
        }
    }

    fn cfg(&self) -> Result<LeafConfig> {
        LeafConfig::load(&self.dirs.leaf())?.ok_or_else(|| {
            Error::Config(
                "this machine has no home yet; run `replicant subscribe <home> <paths...>`".into(),
            )
        })
    }

    fn remote(&self, cfg: &LeafConfig) -> Result<Remote<'_>> {
        let identity = Identity::load(&cfg.credentials(&self.dirs))?;
        Ok(Remote {
            journal: &self.journal,
            address: cfg.home.address.clone(),
            me: cfg.name()?,
            home_dir: cfg.home_dir()?,
            dialer: Dialer::new(&identity, CONNECT_TIMEOUT)?,
        })
    }

    pub fn handle(&self, req: LocalRequest) -> LocalAnswer {
        self.dispatch(req).map_err(|e| e.to_string())
    }

    fn dispatch(&self, req: LocalRequest) -> Result<LocalReply> {
        match req {
            LocalRequest::Ping => Ok(LocalReply::Pong {
                role: "leaf".into(),
                name: self.cfg().and_then(|c| c.name()).unwrap_or_default(),
            }),
            LocalRequest::Subscribe {
                home,
                address,
                rsync_host,
                credentials,
                paths,
                as_local,
            } => self.subscribe(home, address, rsync_host, credentials, paths, as_local),
            LocalRequest::Unsubscribe { paths } => {
                let mut cfg = self.cfg()?;
                let reply = self
                    .remote(&cfg)?
                    .call(HomeRequest::Unsubscribe { paths })?;
                let HomeReply::Unsubscribed(out) = reply else {
                    return Err(Error::Remote(format!("unexpected reply: {reply:?}")));
                };
                let gone: Vec<String> = out.removed.iter().chain(&out.carved).cloned().collect();
                cfg.as_local
                    .retain(|k, _| !gone.iter().any(|g| paths::is_within(k, g)));
                cfg.save(&self.dirs.leaf())?;
                Ok(LocalReply::Unsubscribed(out))
            }
            LocalRequest::Status => Ok(LocalReply::LeafStatus(Box::new(self.status()?))),
            LocalRequest::Fleet => {
                let cfg = self.cfg()?;
                match self.remote(&cfg)?.call(HomeRequest::FleetState)? {
                    HomeReply::Fleet(f) => Ok(LocalReply::Fleet(f)),
                    other => Err(Error::Remote(format!("unexpected reply: {other:?}"))),
                }
            }
            LocalRequest::DryRun { no_wait } => {
                let cfg = self.cfg()?;
                let link = self.remote(&cfg)?;
                let (me, hd, state) = (cfg.name()?, cfg.home_dir()?, self.dirs.state());
                let scratch = self.dirs.scratch();
                let ctx = LeafCtx {
                    me: &me,
                    home_dir: &hd,
                    cfg: &cfg,
                    state_dir: &state,
                    scratch_dir: &scratch,
                };
                Ok(LocalReply::Plan(Box::new(plan::plan(
                    &link, ctx, true, no_wait,
                )?)))
            }
            LocalRequest::Log { filter } => Ok(LocalReply::Log {
                lines: self.journal.read(&filter)?,
            }),
            LocalRequest::Pending
            | LocalRequest::PreviewAllow { .. }
            | LocalRequest::Approve { .. }
            | LocalRequest::Reissue { .. } => Err(Error::Refused(
                "approve runs on home, and this machine is a leaf".into(),
            )),
        }
    }

    fn subscribe(
        &self,
        home: String,
        address: Option<String>,
        rsync_host: Option<String>,
        credentials: Option<String>,
        raw: Vec<String>,
        as_local: Option<PathBuf>,
    ) -> Result<LocalReply> {
        let home_name = short_hostname(&home)
            .ok_or_else(|| Error::Config(format!("{home:?} is not a host name")))?;
        if let Some(local) = &as_local {
            check_local(local)?;
            if raw.len() != 1 {
                return Err(Error::Path("--as maps exactly one path".into()));
            }
        }
        let mut cfg = match LeafConfig::load(&self.dirs.leaf())? {
            Some(c) if c.home.name != home_name => {
                return Err(Error::Refused(format!(
                    "this leaf's home is {}; a leaf talks to exactly one home",
                    c.home.name
                )));
            }
            Some(c) => c,
            None => LeafConfig {
                name: None,
                home_dir: None,
                home: HomeEntry {
                    name: home_name.clone(),
                    address: with_port(&home),
                    credentials: None,
                    rsync_host: None,
                    rsync_timeout_secs: None,
                },
                as_local: Default::default(),
            },
        };
        if let Some(a) = address {
            cfg.home.address = with_port(&a);
        }
        cfg.home.rsync_host = rsync_host.or(cfg.home.rsync_host);
        cfg.home.credentials = credentials.or(cfg.home.credentials);
        cfg.save(&self.dirs.leaf())?;

        let reply = self
            .remote(&cfg)?
            .call(HomeRequest::Subscribe { paths: raw })?;
        let HomeReply::Subscribed(out) = reply else {
            return Err(Error::Remote(format!("unexpected reply: {reply:?}")));
        };
        if let (Some(local), Some(rel)) = (as_local, out.requested.first()) {
            cfg.as_local.insert(rel.clone(), local);
            cfg.save(&self.dirs.leaf())?;
        }
        Ok(LocalReply::Subscribed(out))
    }

    fn status(&self) -> Result<LeafStatus> {
        let cfg = self.cfg()?;
        let link = self.remote(&cfg)?;
        let me = cfg.name()?;
        let view = plan::subscriptions(&link)?;
        let (hd, state, scratch) = (cfg.home_dir()?, self.dirs.state(), self.dirs.scratch());
        let planned = match view.enrollment {
            Enrollment::Enrolled => Some(plan::plan(
                &link,
                LeafCtx {
                    me: &me,
                    home_dir: &hd,
                    cfg: &cfg,
                    state_dir: &state,
                    scratch_dir: &scratch,
                },
                false,
                false,
            )?),
            _ => None,
        };
        let queue = match link.call(HomeRequest::FleetState)? {
            HomeReply::Fleet(f) => f.queue,
            _ => vec![],
        };
        let except = view.specs.iter().flat_map(|s| s.except.clone()).collect();
        Ok(LeafStatus {
            me,
            home: view.home,
            enrollment: view.enrollment,
            cap: view.cap,
            paths: view.specs.into_iter().map(|s| s.rel).collect(),
            except,
            as_local: cfg.as_local,
            pending_paths: view.pending_paths,
            plan: planned,
            queue,
        })
    }
}
