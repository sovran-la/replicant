//! F8 end to end: a real home daemon, a leaf that dials it over mTLS, waits
//! in the queue, and goes away.

use super::*;
use crate::daemon::journal::Filter;
use crate::daemon::queue::QueueView;
use crate::daemon::remote::Dialer;
use crate::daemon::tls::Identity;
use crate::testutil::Bed;
use std::sync::mpsc;
use std::time::Instant;
use wire::{HomeAnswer, HomeCall, HomeReply, HomeRequest, WIRE_VERSION};

struct Leaf {
    dialer: Dialer,
    addr: String,
    hd: std::path::PathBuf,
}

impl Leaf {
    fn call(&self, request: HomeRequest, deadline: Duration) -> Result<HomeReply> {
        let call = HomeCall {
            version: WIRE_VERSION,
            host: "leaf".into(),
            home_dir: self.hd.clone(),
            request,
        };
        let a: HomeAnswer = self.dialer.call(&self.addr, &call, deadline)?;
        a.result.map_err(Error::Remote)
    }

    fn queue(&self) -> Vec<QueueView> {
        match self.call(HomeRequest::FleetState, Duration::from_secs(5)) {
            Ok(HomeReply::Fleet(f)) => f.queue,
            other => panic!("{other:?}"),
        }
    }
}

/// Poll home's queue (a remote state the test cannot be notified about)
/// until `done` holds, or fail after a bound far beyond the 1s lease.
fn until(leaf: &Leaf, what: &str, done: impl Fn(&[QueueView]) -> bool) -> Vec<QueueView> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let q = leaf.queue();
        if done(&q) {
            return q;
        }
        assert!(Instant::now() < deadline, "{what}: queue is {q:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_leaf_that_hangs_up_while_waiting_is_expired_and_never_reaches_the_head() {
    let bed = Bed::enrolled();
    let text = std::fs::read_to_string(bed.home_dirs.fleet()).unwrap();
    std::fs::write(
        bed.home_dirs.fleet(),
        text.replace("listen", "lease_secs = 1\nlisten"),
    )
    .unwrap();
    let home = Daemon::start(bed.home_dirs.clone()).unwrap();
    let id = Identity::load(&super::test_certs::leaf_identity(&bed, "leaf")).unwrap();
    let leaf = Arc::new(Leaf {
        dialer: Dialer::new(&id, Duration::from_secs(5)).unwrap(),
        addr: home.tcp_addr().unwrap().to_string(),
        hd: bed.leaf_hd.clone(),
    });

    let Ok(HomeReply::Lease { lease: head, .. }) = leaf.call(
        HomeRequest::Acquire { no_wait: true },
        Duration::from_secs(5),
    ) else {
        panic!()
    };
    // Keep the head alive for the whole test.
    let (stop, stopped) = mpsc::channel::<()>();
    let l = leaf.clone();
    let beat = std::thread::spawn(move || {
        while stopped.recv_timeout(Duration::from_millis(200)).is_err() {
            l.call(
                HomeRequest::Heartbeat { lease: head },
                Duration::from_secs(5),
            )
            .unwrap();
        }
    });

    // The waiter gives up after 2.5s, more than two of its 1s leases, and its
    // connection closes as it does.
    let l = leaf.clone();
    let waiter = std::thread::spawn(move || {
        l.call(
            HomeRequest::Acquire { no_wait: false },
            Duration::from_millis(2500),
        )
    });
    let q = until(&leaf, "the waiter queues", |q| q.len() == 2);
    let waiting = q[1].id;
    assert!(waiter.join().unwrap().is_err(), "the waiter timed out");
    // Connected, it outlived its lease; this look comes well inside the
    // 1s it has left after its last refresh.
    assert!(
        leaf.queue().iter().any(|e| e.id == waiting),
        "a connected waiter was not kept alive"
    );

    // Hung up: no longer refreshed, so the reaper expires it.
    let q = until(&leaf, "the waiter is expired", |q| {
        !q.iter().any(|e| e.id == waiting)
    });
    assert_eq!(q.len(), 1);
    assert_eq!(q[0].id, head, "the head is untouched");
    let LocalReply::Log { lines } = local::call(
        &bed.home_dirs,
        LocalRequest::Log {
            filter: Filter::default(),
        },
    )
    .unwrap() else {
        panic!()
    };
    let expired: Vec<_> = lines.iter().filter(|l| l.event == "expired").collect();
    assert_eq!(expired.len(), 1, "{lines:?}");
    assert!(
        expired[0].detail.contains("waiting"),
        "expired before reaching the head: {}",
        expired[0].detail
    );

    stop.send(()).unwrap();
    beat.join().unwrap();
    leaf.call(HomeRequest::Release { lease: head }, Duration::from_secs(5))
        .unwrap();
    assert!(leaf.queue().is_empty());
    home.stop();
}
