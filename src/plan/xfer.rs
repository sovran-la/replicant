//! The data-plane seam: how a unit's dry run and transfer run. Everything
//! around it (markers, baselines, guardrails, the digest, the journal)
//! reads only [`Itemized`] and [`Applied`].

use super::{HomeLink, LeafCtx};
use crate::Result;
use crate::abort::Abort;
use crate::changes::Itemized;
use crate::native::transfer::{Counts, Diffed, Env};
use std::time::Duration;

/// How the data plane runs: the daemons move the bytes over their mTLS
/// link (`native`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plane {
    /// How long a side waits for an ack before it calls a data connection
    /// dead. [`crate::native::ACK_TIMEOUT`]; tests may shorten it.
    pub ack_timeout: Duration,
}

impl Default for Plane {
    fn default() -> Self {
        Self {
            ack_timeout: crate::native::ACK_TIMEOUT,
        }
    }
}

/// One unit's transfer.
pub type Xfer = crate::native::transfer::Transfer;

/// What a transfer did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// What it changed on the target, also when it then failed.
    pub itemized: Itemized,
    /// Why it did not finish; `None`: done and verified.
    pub failure: Option<String>,
    /// Stopped by its abort (a lost lease, the CLI gone).
    pub stopped: bool,
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

    fn env(&self) -> Env<'_> {
        Env {
            link: self.link,
            me: self.ctx.me,
            lease: self.lease,
            abort: self.abort,
            streams: self.streams,
            ack_timeout: self.ctx.plane.ack_timeout,
            progress: self.progress,
        }
    }

    /// The transfer's dry run: what it would change, and the change list.
    pub fn dry_run(&self, x: &Xfer) -> Result<Diffed> {
        x.dry_run(&self.env())
    }

    /// Carry the transfer out, as `fresh`, its dry run just before, says.
    pub fn apply(&self, x: &Xfer, fresh: &Diffed) -> Applied {
        let ran = x.apply(&self.env(), fresh);
        Applied {
            failure: (!ran.ok()).then(|| ran.why()),
            stopped: ran.stopped,
            itemized: ran.itemized,
        }
    }
}
