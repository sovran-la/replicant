//! F8: a waiter's lease lives only as long as its connection.

use super::*;

/// A waiter whose connection probe is a flag, reporting each call's instant
/// and answer down `probes`.
fn waiter(
    q: &Arc<Queue>,
    host: &'static str,
    lease: Duration,
    connected: &Arc<std::sync::atomic::AtomicBool>,
    probes: mpsc::Sender<(Instant, bool)>,
) -> std::thread::JoinHandle<Result<u64>> {
    let (q, connected) = (q.clone(), connected.clone());
    std::thread::spawn(move || {
        let id = q.enqueue(host, EntryKind::DryRun, vec!["work".into()], lease, false)?;
        q.wait(id, lease, LONG, &|| {
            let c = connected.load(std::sync::atomic::Ordering::SeqCst);
            let _ = probes.send((Instant::now(), c));
            c
        })?;
        Ok(id)
    })
}

/// Wait for the next probe that answered `want`.
fn probe(rx: &mpsc::Receiver<(Instant, bool)>, want: bool) -> Instant {
    loop {
        let (at, c) = rx.recv_timeout(LONG).expect("the waiter probes every tick");
        if c == want {
            return at;
        }
    }
}

/// F8: a waiter whose connection is gone stops refreshing its lease, so the
/// next sweep expires it; while connected, it keeps it alive.
#[test]
fn a_disconnected_waiter_is_expired_by_the_next_sweep() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let q = Arc::new(Queue::new());
    let head = take(&q, "head");
    let lease = Duration::from_secs(5);
    let connected = Arc::new(AtomicBool::new(true));
    let (tx, rx) = mpsc::channel();
    let w = waiter(&q, "gone", lease, &connected, tx);

    // Connected: refreshed after this probe, so alive until at least p + lease.
    let p = probe(&rx, true);
    assert!(q.expire(p + lease - Duration::from_millis(1)).is_empty());

    connected.store(false, Ordering::SeqCst);
    // Every refresh happened before this probe; none after it.
    let gone = probe(&rx, false);
    let expired = q.expire(gone + lease);
    assert_eq!(
        expired.iter().map(|e| e.host.as_str()).collect::<Vec<_>>(),
        ["gone"]
    );
    assert!(expired[0].started_at.is_none(), "it never reached the head");
    let err = w.join().unwrap().unwrap_err().to_string();
    assert!(err.contains("removed while waiting"), "{err}");
    assert_eq!(q.view().len(), 1, "the head is untouched");
    q.release(head).unwrap();
}

/// F8: if the head is released before the sweep, a disconnected waiter is
/// withdrawn rather than granted a lease nobody would heartbeat; the entry
/// behind it proceeds at once.
#[test]
fn a_disconnected_waiter_that_reaches_the_head_is_withdrawn_not_granted() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let q = Arc::new(Queue::new());
    let head = take(&q, "head");
    let connected = Arc::new(AtomicBool::new(true));
    let (tx, rx) = mpsc::channel();
    let gone = waiter(&q, "gone", LEASE, &connected, tx);
    probe(&rx, true);
    let q2 = q.clone();
    let behind = std::thread::spawn(move || take(&q2, "behind"));
    while q.view().len() < 3 {
        std::thread::yield_now();
    }
    connected.store(false, Ordering::SeqCst);
    probe(&rx, false);

    q.release(head).unwrap();
    let err = gone.join().unwrap().unwrap_err().to_string();
    assert!(err.contains(WAITER_GONE), "{err}");
    // With a 30s lease, `behind` would wait that long if `gone` were granted.
    let id = behind.join().unwrap();
    let v = q.view();
    assert_eq!(v.len(), 1);
    assert_eq!((v[0].id, v[0].host.as_str()), (id, "behind"));
    assert!(v[0].started_at.is_some());
    q.release(id).unwrap();
}

#[test]
fn a_disconnected_caller_is_not_granted_an_empty_queue() {
    let q = Queue::new();
    let id = q
        .enqueue("gone", EntryKind::DryRun, vec![], LEASE, false)
        .unwrap();
    let err = q.wait(id, LEASE, LONG, &|| false).unwrap_err().to_string();
    assert!(err.contains(WAITER_GONE), "{err}");
    assert!(q.view().is_empty());
}
