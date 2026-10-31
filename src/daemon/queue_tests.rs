use super::*;
use std::sync::Arc;
use std::sync::mpsc;

const LEASE: Duration = Duration::from_secs(30);
const LONG: Duration = Duration::from_secs(10);

fn take(q: &Queue, host: &str) -> u64 {
    q.acquire(
        host,
        EntryKind::DryRun,
        vec!["work".into()],
        LEASE,
        false,
        LONG,
    )
    .unwrap()
}

#[test]
fn empty_queue_grants_immediately() {
    let q = Queue::new();
    let id = take(&q, "a");
    let v = q.view();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].id, id);
    assert!(v[0].started_at.is_some());
    q.release(id).unwrap();
    assert!(q.view().is_empty());
}

#[test]
fn fifo_order_is_strict() {
    let q = Arc::new(Queue::new());
    let first = take(&q, "first");
    let (tx, rx) = mpsc::channel();
    let mut handles = Vec::new();
    for name in ["second", "third"] {
        // Enqueue in a known order: wait until the previous waiter is queued.
        let before = q.view().len();
        let (q2, tx2) = (q.clone(), tx.clone());
        handles.push(std::thread::spawn(move || {
            let id = take(&q2, name);
            tx2.send(name).unwrap();
            q2.release(id).unwrap();
        }));
        while q.view().len() == before {
            std::thread::yield_now();
        }
    }
    assert!(
        rx.try_recv().is_err(),
        "nobody proceeds while the head is held"
    );
    let views = q.view();
    assert_eq!(
        views.iter().map(|v| v.host.as_str()).collect::<Vec<_>>(),
        ["first", "second", "third"]
    );
    assert!(views[1].started_at.is_none());
    q.release(first).unwrap();
    assert_eq!(rx.recv().unwrap(), "second");
    assert_eq!(rx.recv().unwrap(), "third");
    for h in handles {
        h.join().unwrap();
    }
}

#[test]
fn no_wait_fails_with_who_is_ahead_and_does_not_enqueue() {
    let q = Queue::new();
    let _held = take(&q, "twinoaks");
    let err = q
        .acquire("birchbook", EntryKind::DryRun, vec![], LEASE, true, LONG)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("queue busy") && err.contains("twinoaks dry-run work"),
        "{err}"
    );
    assert_eq!(q.view().len(), 1);
}

#[test]
fn head_that_stops_heartbeating_expires_and_releases_the_queue() {
    let q = Arc::new(Queue::new());
    let dead = q
        .acquire(
            "dead",
            EntryKind::DryRun,
            vec!["work".into()],
            Duration::from_millis(50),
            false,
            LONG,
        )
        .unwrap();
    let q2 = q.clone();
    let (tx, rx) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let id = take(&q2, "next");
        tx.send(id).unwrap();
    });
    while q.view().len() < 2 {
        std::thread::yield_now();
    }
    assert!(
        q.expire(Instant::now()).is_empty(),
        "nothing has expired yet"
    );
    let expired = q.expire(Instant::now() + Duration::from_millis(100));
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].id, dead);
    let next = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the next entry proceeds");
    waiter.join().unwrap();
    assert!(
        q.heartbeat(dead, LEASE).is_err(),
        "an expired lease cannot be revived"
    );
    assert!(q.release(dead).is_err());
    q.release(next).unwrap();
}

#[test]
fn heartbeat_keeps_a_lease_alive() {
    let q = Queue::new();
    let id = q
        .acquire(
            "a",
            EntryKind::Hold,
            vec![],
            Duration::from_millis(100),
            false,
            LONG,
        )
        .unwrap();
    q.heartbeat(id, Duration::from_secs(60)).unwrap();
    assert!(q.expire(Instant::now() + Duration::from_secs(1)).is_empty());
}

#[test]
fn waiters_keep_their_own_lease_alive_while_waiting() {
    let q = Arc::new(Queue::new());
    let head = take(&q, "head");
    let q2 = q.clone();
    let waiter = std::thread::spawn(move || {
        q2.acquire(
            "w",
            EntryKind::DryRun,
            vec![],
            Duration::from_millis(300),
            false,
            LONG,
        )
    });
    while q.view().len() < 2 {
        std::thread::yield_now();
    }
    std::thread::sleep(Duration::from_millis(600));
    // The waiter refreshes every tick, so an expiry sweep now finds only
    // entries that truly stopped; the head's 30s lease is also alive.
    assert!(q.expire(Instant::now()).is_empty());
    q.release(head).unwrap();
    let id = waiter.join().unwrap().unwrap();
    q.release(id).unwrap();
}

#[test]
fn max_wait_withdraws_the_entry() {
    let q = Queue::new();
    let _head = take(&q, "head");
    let err = q
        .acquire(
            "w",
            EntryKind::DryRun,
            vec![],
            LEASE,
            false,
            Duration::from_millis(50),
        )
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("gave up waiting") && err.contains("head"),
        "{err}"
    );
    assert_eq!(q.view().len(), 1);
}

#[test]
fn describe_names_host_kind_paths_and_age() {
    let v = QueueView {
        id: 1,
        host: "twinoaks".into(),
        kind: EntryKind::Hold,
        paths: vec!["work/tundra".into()],
        enqueued_at: 0,
        started_at: None,
    };
    assert_eq!(describe(&v), "twinoaks hold work/tundra, waiting");
}
