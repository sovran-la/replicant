//! The data-plane seam: how a unit's dry run and transfer run. Everything
//! around it (markers, baselines, guardrails, the digest, the journal)
//! reads only [`Itemized`] and [`Applied`], whichever plane produced them.

use super::{HomeLink, LeafCtx};
use crate::abort::Abort;
use crate::changes::Itemized;
use crate::marker::Direction;
use crate::native::transfer::{Counts, Diffed, Env};
use crate::rsync::{self, Client, Runner, Stopped};
use crate::{Error, Result};
use std::time::Duration;

/// Which data plane moves the bytes.
#[derive(Debug, Clone, Copy)]
pub enum Plane<'a> {
    /// rsync, this program (`rsync` on `PATH`; tests stand a script in).
    Rsync(&'a str),
    /// The daemons, over their mTLS link.
    Native { ack_timeout: Duration },
}

impl Plane<'_> {
    pub fn native() -> Self {
        Plane::Native {
            ack_timeout: crate::native::ACK_TIMEOUT,
        }
    }
}

/// The plane a test runs on unless it says: native, or rsync when
/// `REPLICANT_TEST_PLANE=rsync` (both planes pass the suite).
#[cfg(test)]
pub fn test_plane() -> Plane<'static> {
    match std::env::var("REPLICANT_TEST_PLANE").as_deref() {
        Ok("rsync") => Plane::Rsync(super::RSYNC),
        _ => Plane::native(),
    }
}

/// One unit's transfer on one plane.
#[derive(Debug, Clone)]
pub enum Xfer {
    Rsync(rsync::Transfer),
    Native(crate::native::transfer::Transfer),
}

impl Xfer {
    pub fn direction(&self) -> Direction {
        match self {
            Xfer::Rsync(t) => t.direction,
            Xfer::Native(t) => t.direction,
        }
    }
    pub fn is_file(&self) -> bool {
        match self {
            Xfer::Rsync(t) => t.is_file,
            Xfer::Native(t) => t.is_file,
        }
    }
    pub fn source_absent(&self) -> bool {
        match self {
            Xfer::Rsync(t) => t.source_absent,
            Xfer::Native(t) => t.source_absent,
        }
    }
}

/// What a transfer did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// What it changed on the target, also when it then failed.
    pub itemized: Itemized,
    /// Why it did not finish; `None`: done.
    pub failure: Option<String>,
    /// Stopped by its abort (a lost lease, the CLI gone).
    pub stopped: bool,
    /// What the plane calls being stopped: `rsync stopped`.
    pub stop_word: &'static str,
}

impl Applied {
    pub fn ok(&self) -> bool {
        self.failure.is_none()
    }
    pub fn why(&self) -> String {
        self.failure.clone().unwrap_or_else(|| "done".into())
    }
}

/// What a dry run or transfer runs with, besides the transfer.
pub(super) struct Ops<'a> {
    pub link: &'a dyn HomeLink,
    pub ctx: LeafCtx<'a>,
    pub lease: u64,
    pub abort: &'a Abort,
    pub streams: u32,
    pub progress: &'a (dyn Fn(Counts) + Sync),
}

fn ignore(_: Counts) {}

impl<'a> Ops<'a> {
    pub fn new(
        link: &'a dyn HomeLink,
        ctx: LeafCtx<'a>,
        lease: u64,
        abort: &'a Abort,
        streams: u32,
    ) -> Self {
        Self {
            link,
            ctx,
            lease,
            abort,
            streams,
            progress: &ignore,
        }
    }

    fn runner(&self, program: &str) -> Result<Runner> {
        Ok(Runner {
            program: program.into(),
            abort: self.abort.clone(),
            ..Runner::new(
                self.ctx.cfg.rsync_timeout(),
                Client::detect()?,
                self.ctx.scratch_dir.to_path_buf(),
            )
        })
    }

    fn env(&self, ack_timeout: Duration) -> Env<'_> {
        Env {
            link: self.link,
            me: self.ctx.me,
            lease: self.lease,
            abort: self.abort,
            streams: self.streams,
            ack_timeout,
            progress: self.progress,
        }
    }

    /// The transfer's dry run, and for the native plane the change list it
    /// would carry out.
    pub fn dry_run(&self, x: &Xfer) -> Result<(Itemized, Option<Diffed>)> {
        match (x, self.ctx.plane) {
            (Xfer::Rsync(t), Plane::Rsync(p)) => Ok((t.dry_run(&self.runner(p)?)?, None)),
            (Xfer::Native(t), Plane::Native { ack_timeout }) => {
                let d = t.dry_run(&self.env(ack_timeout))?;
                Ok((d.itemized.clone(), Some(d)))
            }
            _ => Err(Error::Transfer(
                "a transfer planned on another plane".into(),
            )),
        }
    }

    /// Carry the transfer out; `fresh` is its native dry run just before,
    /// if there was one (a push's), else one is taken now.
    pub fn apply(&self, x: &Xfer, fresh: Option<&Diffed>) -> Result<Applied> {
        match (x, self.ctx.plane) {
            (Xfer::Rsync(t), Plane::Rsync(p)) => {
                let ran = t.apply(&self.runner(p)?)?;
                Ok(Applied {
                    failure: (!ran.ok()).then(|| ran.why()),
                    stopped: ran.stopped == Some(Stopped::Aborted),
                    itemized: ran.itemized,
                    stop_word: "rsync stopped",
                })
            }
            (Xfer::Native(t), Plane::Native { ack_timeout }) => {
                let env = self.env(ack_timeout);
                let taken;
                let d = match fresh {
                    Some(d) => d,
                    None => {
                        taken = t.dry_run(&env)?;
                        &taken
                    }
                };
                let ran = t.apply(&env, d);
                Ok(Applied {
                    failure: (!ran.ok()).then(|| ran.why()),
                    stopped: ran.stopped,
                    itemized: ran.itemized,
                    stop_word: "transfer stopped",
                })
            }
            _ => Err(Error::Transfer(
                "a transfer planned on another plane".into(),
            )),
        }
    }
}
