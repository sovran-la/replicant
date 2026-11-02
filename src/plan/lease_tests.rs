//! The heartbeat loop against a scripted home: what counts as lost, what is
//! retried, and that a lost lease fails the plan's result.

use super::*;
use crate::daemon::wire::HomeReply;
use std::collections::VecDeque;

/// Answers heartbeats from a script (then `then` forever) and counts them.
struct Scripted {
    script: Mutex<VecDeque<Result<HomeReply>>>,
    then: fn() -> Result<HomeReply>,
    heartbeats: Mutex<usize>,
    release: fn() -> Result<HomeReply>,
}

fn ok() -> Result<HomeReply> {
    Ok(HomeReply::Done)
}
fn unreachable() -> Result<HomeReply> {
    Err(Error::Remote(
        "home at h:7750 is unreachable: connection refused".into(),
    ))
}
fn gone() -> Result<HomeReply> {
    Err(Error::Remote(format!(
        "refused: lease 7 {NOT_QUEUED} (expired?)"
    )))
}

impl Scripted {
    fn new(script: Vec<Result<HomeReply>>, then: fn() -> Result<HomeReply>) -> Self {
        Self {
            script: Mutex::new(script.into()),
            then,
            heartbeats: Mutex::new(0),
            release: ok,
        }
    }
    fn count(&self) -> usize {
        *self.heartbeats.lock().unwrap()
    }
}

impl HomeLink for Scripted {
    fn call(&self, request: HomeRequest) -> Result<HomeReply> {
        match request {
            HomeRequest::Heartbeat { lease } => {
                assert_eq!(lease, 7);
                *self.heartbeats.lock().unwrap() += 1;
                let next = self.script.lock().unwrap().pop_front();
                next.unwrap_or_else(self.then)
            }
            HomeRequest::Release { lease } => {
                assert_eq!(lease, 7);
                (self.release)()
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}

/// Wait, bounded, for `cond`; the bound is a failure, not a pass.
fn until(what: &str, cond: impl Fn() -> bool) {
    let by = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < by, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn no_lease_runs_f_without_heartbeats() {
    let link = Scripted::new(vec![], || panic!("no heartbeat without a lease"));
    let r = with_heartbeat(&link, None, Abort::default(), |w| {
        w.check()?;
        Ok(5)
    });
    assert_eq!(r.unwrap(), 5);
}

#[test]
fn healthy_heartbeats_keep_the_plan_alive() {
    let link = Scripted::new(vec![], ok);
    let r = with_heartbeat(&link, Some((7, 1)), Abort::default(), |w| {
        until("three heartbeats", || link.count() >= 3);
        w.check()?;
        Ok("done")
    });
    assert_eq!(r.unwrap(), "done");
}

#[test]
fn transient_failures_are_retried_within_the_lease() {
    let link = Scripted::new(vec![unreachable(), unreachable()], ok);
    let r = with_heartbeat(&link, Some((7, 1)), Abort::default(), |w| {
        until("recovery after two failures", || link.count() >= 4);
        w.check()?;
        Ok(())
    });
    assert!(r.is_ok(), "{r:?}");
}

#[test]
fn not_in_the_queue_is_lost_at_once_and_fails_a_successful_plan() {
    let link = Scripted::new(vec![ok(), gone()], || panic!("no heartbeat after loss"));
    let r = with_heartbeat(&link, Some((7, 1)), Abort::default(), |w| {
        until("the loss", || w.check().is_err());
        Ok(())
    });
    let err = r.unwrap_err().to_string();
    assert!(err.contains("lease lost"), "{err}");
    assert!(err.contains("entry 7") && err.contains(NOT_QUEUED), "{err}");
    assert_eq!(link.count(), 2);
}

#[test]
fn loss_overrides_whatever_f_returned() {
    let link = Scripted::new(vec![gone()], ok);
    let r: Result<()> = with_heartbeat(&link, Some((7, 1)), Abort::default(), |w| {
        until("the loss", || w.check().is_err());
        Err(Error::Rsync("something else".into()))
    });
    assert!(r.unwrap_err().to_string().contains("lease lost"));
}

#[test]
fn unreachable_for_the_whole_lease_is_lost() {
    let link = Scripted::new(vec![], unreachable);
    let started = Instant::now();
    let r = with_heartbeat(&link, Some((7, 1)), Abort::default(), |w| {
        until("the loss", || w.check().is_err());
        Ok(())
    });
    let err = r.unwrap_err().to_string();
    assert!(
        err.contains("lease lost") && err.contains("for 1s"),
        "{err}"
    );
    assert!(started.elapsed() >= Duration::from_secs(1));
    assert!(
        link.count() >= 3,
        "retried before giving up: {}",
        link.count()
    );
}

#[test]
fn heartbeats_stop_when_f_returns() {
    let link = Scripted::new(vec![], ok);
    with_heartbeat(&link, Some((7, 1)), Abort::default(), |_| Ok(())).unwrap();
    let n = link.count();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(link.count(), n, "the thread ended with the plan");
}

#[test]
fn a_failed_release_is_an_error() {
    let mut link = Scripted::new(vec![], ok);
    assert!(release(&link, 7).is_ok());
    link.release = gone;
    let err = release(&link, 7).unwrap_err().to_string();
    assert!(err.contains("could not release queue entry 7"), "{err}");
}

#[test]
fn an_outside_abort_is_the_watch_s_and_does_not_void_the_plan() {
    for lease in [None, Some((7, 1))] {
        let link = Scripted::new(vec![], ok);
        let hangup = Abort::default();
        let r = with_heartbeat(&link, lease, hangup.clone(), |w| {
            assert!(!w.abort().raised());
            hangup.raise_because("the CLI hung up");
            assert!(w.abort().raised(), "the rsync's flag is the one raised");
            assert_eq!(w.abort().why().as_deref(), Some("the CLI hung up"));
            w.check()?;
            Ok(1)
        });
        assert_eq!(r.unwrap(), 1, "only a lost lease voids a plan");
    }
}

#[test]
fn a_lost_lease_raises_the_outside_abort_too() {
    let link = Scripted::new(vec![gone()], ok);
    let outside = Abort::default();
    let r = with_heartbeat(&link, Some((7, 1)), outside.clone(), |w| {
        until("the loss", || w.check().is_err());
        Ok(())
    });
    assert!(r.is_err());
    assert!(
        outside.raised() && outside.why().is_none(),
        "reads as a lost lease"
    );
}
