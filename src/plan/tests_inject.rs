//! Making one unit's transfer go wrong, on whichever plane the suite runs
//! (`xfer::test_plane`): on the rsync plane a script stands in for rsync
//! for that unit's real run; on the native plane the unit's data
//! connection is cut or stalled, or a request around it fails. The tests
//! that use it assert the same outcome either way.

use super::Plane;
use crate::daemon::home::Home;
use crate::daemon::wire::HomeRequest;
use crate::native::tests_fixture::{Fault, Faulty};
use crate::testutil::{self, Bed, InProcess};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// What goes wrong with the unit's transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum How {
    /// It fails before writing anything.
    Fails,
    /// It is killed partway.
    Killed,
    /// It hangs until something stops it.
    Stalls,
    /// It writes everything, then reports failure.
    WritesThenFails,
}

pub(crate) struct Inject {
    pub unit: String,
    pub how: How,
    /// The rsync plane's script, if that is the plane.
    script: Option<String>,
    /// Where the script writes its pid and its child's.
    pid: PathBuf,
    kid: PathBuf,
    /// The unit's transfer is a deletion carried out here (a removal, a
    /// "deleted on home"): natively local, with no connection to cut.
    pub deleting: bool,
}

impl Inject {
    pub fn new(bed: &Bed, unit: &str, how: How) -> Self {
        let (pid, kid) = (
            bed.tmp.path().join("rsync.pid"),
            bed.tmp.path().join("rsync.kid"),
        );
        let then = match how {
            How::Fails => "echo 'rsync: connection lost' >&2; exit 23".to_string(),
            How::Killed | How::Stalls => format!(
                "echo $$ > {}; sleep 30 & echo $! > {}; wait; exit 0",
                pid.display(),
                kid.display()
            ),
            How::WritesThenFails => "rsync \"$@\"; exit 23".into(),
        };
        // A real run (no --dry-run) of exactly this unit: its argument ends
        // in the unit's path and a slash.
        let body = format!(
            "for a in \"$@\"; do [ \"$a\" = --dry-run ] && exec rsync \"$@\"; done\n\
             for a in \"$@\"; do case \"$a\" in */{unit}/) {then} ;; esac; done\n\
             exec rsync \"$@\""
        );
        let script = match super::xfer::test_plane() {
            Plane::Rsync(_) => Some(
                testutil::script(bed.tmp.path(), "rsync-inject", &body)
                    .display()
                    .to_string(),
            ),
            Plane::Native { .. } => None,
        };
        Self {
            unit: unit.into(),
            how,
            script,
            pid,
            kid,
            deleting: false,
        }
    }

    /// The transfer is a deletion carried out here: natively it fails as a
    /// deletion does, on a directory it may not write (`sync_inject`).
    pub fn deleting(mut self) -> Self {
        self.deleting = true;
        self
    }

    pub fn native(&self) -> bool {
        self.script.is_none()
    }

    /// The plane the sync runs on, the script standing in on rsync's.
    pub fn plane(&self) -> Plane<'_> {
        match &self.script {
            Some(s) => Plane::Rsync(s),
            None => super::xfer::test_plane(),
        }
    }

    /// The link to home: faulty for the unit on the native plane.
    pub fn link<'a>(&'a self, bed: &Bed, home: &'a Home) -> Faulty<'a> {
        let inner = InProcess {
            home,
            host: "leaf".into(),
            home_dir: bed.leaf_hd.clone(),
        };
        let fault = match (self.native(), self.how) {
            (false, _) | (true, How::WritesThenFails) => Fault::None,
            (true, How::Fails) => Fault::CutAfter(0, 0),
            (true, How::Killed) => Fault::CutAfter(0, 400),
            (true, How::Stalls) => Fault::Stall(0),
        };
        let link = Faulty::new(inner, fault).at(&self.unit);
        if !(self.native() && self.how == How::WritesThenFails) {
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
        match (self.native(), self.how) {
            (false, How::Fails | How::WritesThenFails) => "rsync exit 23",
            (false, How::Killed) => "killed by a signal",
            (true, How::Fails | How::Killed) => "closed the data connection",
            (true, How::WritesThenFails) => "injected",
            (_, How::Stalls) => "stopped",
        }
    }

    /// Block until the unit's transfer is running (and, for `Killed` on
    /// the rsync plane, kill it).
    pub fn wait_running(&self, link: &Faulty<'_>) {
        match self.native() {
            false => {
                let p = crate::plan::tests::lease_kill::wait_for_pid(&self.pid);
                if self.how == How::Killed {
                    // SAFETY: signalling a test's own child script.
                    unsafe { libc::kill(p, libc::SIGKILL) };
                }
            }
            true if self.how != How::Stalls => {}
            true => {
                let by = Instant::now() + Duration::from_secs(20);
                while !link.stalled.load(Ordering::SeqCst) {
                    assert!(Instant::now() < by, "the stalled transfer never began");
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }

    /// The stalled transfer was stopped: its processes are gone (rsync), or
    /// its connection let go (native).
    pub fn assert_stopped(&self, link: &Faulty<'_>) {
        match self.native() {
            false => {
                for f in [&self.pid, &self.kid] {
                    let p = crate::plan::tests::lease_kill::wait_for_pid(f);
                    let gone = crate::plan::tests::lease_kill::gone(p);
                    assert!(gone, "{} {p} survived", f.display());
                }
            }
            true => {
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
    }
}
