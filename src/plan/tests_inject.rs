//! Making one unit's transfer go wrong: its data connection cut or
//! stalled, or a request around it failing. The tests that use it assert
//! what a unit that does not finish leaves behind.

use crate::daemon::home::Home;
use crate::daemon::wire::HomeRequest;
use crate::native::tests_fixture::{Fault, Faulty};
use crate::testutil::{Bed, InProcess};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// What goes wrong with the unit's transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum How {
    /// It fails before writing anything: the connection is cut at once.
    Fails,
    /// It dies partway: the connection is cut after a few hundred bytes.
    Killed,
    /// It hangs until something stops it: home never answers.
    Stalls,
    /// It writes everything, then fails: home's manifest for its
    /// verification is refused.
    WritesThenFails,
}

pub(crate) struct Inject {
    pub unit: String,
    pub how: How,
    /// The unit's transfer is a deletion carried out here (a removal, a
    /// "deleted on home"): local, with no connection to cut (`sync_inject`).
    pub deleting: bool,
}

impl Inject {
    pub fn new(_bed: &Bed, unit: &str, how: How) -> Self {
        Self {
            unit: unit.into(),
            how,
            deleting: false,
        }
    }

    /// The transfer is a deletion carried out here: it fails as a deletion
    /// does, on a directory it may not write (`sync_inject`).
    pub fn deleting(mut self) -> Self {
        self.deleting = true;
        self
    }

    /// The link to home, faulty for the unit.
    pub fn link<'a>(&'a self, bed: &Bed, home: &'a Home) -> Faulty<'a> {
        let inner = InProcess {
            home,
            host: "leaf".into(),
            home_dir: bed.leaf_hd.clone(),
        };
        let fault = match self.how {
            How::WritesThenFails => Fault::None,
            How::Fails => Fault::CutAfter(0, 0),
            How::Killed => Fault::CutAfter(0, 400),
            How::Stalls => Fault::Stall(0),
        };
        let link = Faulty::new(inner, fault).at(&self.unit);
        if self.how != How::WritesThenFails {
            return link;
        }
        // Once the unit's data has gone over, its verification fails.
        let unit = self.unit.clone();
        link.on_call(Box::new(move |r, opened| match r {
            HomeRequest::Manifest { rel, .. } if *rel == unit && opened > 0 => Err(
                crate::Error::Remote("injected: home's manifest after the transfer".into()),
            ),
            _ => Ok(()),
        }))
    }

    /// What the journal and the outcome say about it.
    pub fn why(&self) -> &'static str {
        match self.how {
            How::Fails | How::Killed => "closed the data connection",
            How::WritesThenFails => "injected",
            How::Stalls => "stopped",
        }
    }

    /// Block until a stalled transfer is running.
    pub fn wait_running(&self, link: &Faulty<'_>) {
        let by = Instant::now() + Duration::from_secs(20);
        while !link.stalled.load(Ordering::SeqCst) {
            assert!(Instant::now() < by, "the stalled transfer never began");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The stalled transfer was stopped: its connection was let go.
    pub fn assert_stopped(&self, link: &Faulty<'_>) {
        let by = Instant::now() + Duration::from_secs(5);
        while !link.released.load(Ordering::SeqCst) {
            assert!(
                Instant::now() < by,
                "the stalled connection was never let go"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
