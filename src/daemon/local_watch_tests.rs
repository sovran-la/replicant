//! The watch on the CLI's end: it always ends with the request, tells a
//! hangup from a quiet CLI, and never blocks in a read (the macOS race of a
//! read with `SO_RCVTIMEO` and a `shutdown` from another thread).

use super::*;
use crate::abort::Abort;
use std::io::{PipeWriter, Write};
use std::os::fd::AsRawFd;
use std::sync::Mutex;
use std::sync::mpsc::{self, RecvTimeoutError};

/// Long enough that only a stuck watch takes it; the limit on a test here.
const STUCK: Duration = Duration::from_secs(20);

/// Run `f` on a thread this test can give up on: a stuck watch cannot be
/// joined, and the failure should be a message, not a suite that never ends.
fn within<T: Send + 'static>(bound: Duration, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (done, finished) = mpsc::channel();
    let running = std::thread::spawn(move || {
        let _ = done.send(f());
    });
    match finished.recv_timeout(bound) {
        Ok(out) => {
            running.join().unwrap();
            out
        }
        Err(RecvTimeoutError::Disconnected) => {
            std::panic::resume_unwind(running.join().unwrap_err())
        }
        Err(RecvTimeoutError::Timeout) => panic!("still stuck after {bound:?}"),
    }
}

/// A CLI that waits for its answer, while the handler holds on for about one
/// `WATCH_POLL`: a stop landing just as a read's `SO_RCVTIMEO` expired was
/// what stranded the old watch.
fn patient_round(i: usize) {
    let (mut cli, daemon) = UnixStream::pair().unwrap();
    let hold = WATCH_POLL + Duration::from_micros(100) * (i % 20) as u32;
    let raised = AtomicBool::new(false);
    std::thread::scope(|s| {
        s.spawn(|| {
            serve(daemon, &|_, p| {
                p.emit(started(i));
                std::thread::sleep(hold);
                raised.store(p.abort().raised(), Ordering::SeqCst);
                Ok(LocalReply::Pong {
                    role: "leaf".into(),
                    name: "n".into(),
                })
            })
        });
        request(&mut cli, LOCAL_WIRE_VERSION);
        let mut seen = Vec::new();
        let reply = read_answer(&mut BufReader::new(&cli), &mut |e| seen.push(e)).unwrap();
        assert!(matches!(reply, LocalReply::Pong { .. }));
        assert_eq!(seen, vec![started(i)]);
    });
    assert!(!raised.load(Ordering::SeqCst), "round {i}: no hangup");
}

/// A CLI that goes away while the handler runs.
fn hangup_round(i: usize) {
    let (mut cli, daemon) = UnixStream::pair().unwrap();
    request(&mut cli, LOCAL_WIRE_VERSION);
    let why = Mutex::new(None);
    std::thread::scope(|s| {
        s.spawn(|| {
            serve(daemon, &|_, p| {
                let by = Instant::now() + Duration::from_secs(10);
                while !p.abort().raised() && Instant::now() < by {
                    std::thread::sleep(Duration::from_millis(1));
                }
                *why.lock().unwrap() = p.abort().why();
                p.emit(started(i));
                Err("stopped".into())
            })
        });
        std::thread::sleep(Duration::from_micros(200) * (i % 10) as u32);
        drop(cli);
    });
    let why = why.into_inner().unwrap();
    assert_eq!(why.as_deref(), Some(HANGUP), "round {i}");
}

#[test]
fn the_watch_always_ends_with_the_request_and_tells_a_hangup_from_patience() {
    const ROUNDS: usize = 200;
    const AT_ONCE: usize = 16;
    within(Duration::from_secs(60), || {
        for batch in (0..ROUNDS).collect::<Vec<_>>().chunks(AT_ONCE) {
            std::thread::scope(|s| {
                for &i in batch {
                    s.spawn(move || {
                        if i % 2 == 0 {
                            patient_round(i);
                        } else {
                            hangup_round(i);
                        }
                    });
                }
            });
        }
    });
}

#[test]
fn the_socket_gets_no_read_timeout_and_is_not_shut() {
    let (mut cli, daemon) = UnixStream::pair().unwrap();
    watched(&daemon, |_| {
        // The watch has had time to set one, were it going to.
        std::thread::sleep(WATCH_POLL / 5);
        assert_eq!(daemon.read_timeout().unwrap(), None);
    });
    assert_eq!(daemon.read_timeout().unwrap(), None);
    // The stop is no `shutdown`: the socket still works, in both directions.
    cli.write_all(b"x").unwrap();
    let mut got = [0u8; 1];
    let mut r = &daemon;
    std::io::Read::read_exact(&mut r, &mut got).unwrap();
    assert_eq!(&got, b"x");
}

#[test]
fn a_panicking_handler_does_not_hold_the_watch() {
    let (cli, daemon) = UnixStream::pair().unwrap();
    let panicked = within(STUCK, move || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            watched(&daemon, |_| panic!("the handler's bug"))
        }))
        .is_err()
    });
    assert!(panicked);
    drop(cli);
}

/// One watch, on a thread: its abort, and what ends it from outside.
struct Watch {
    abort: Abort,
    over: std::sync::Arc<AtomicBool>,
    wake: Option<PipeWriter>,
    ended: mpsc::Receiver<()>,
}

impl Watch {
    fn on(daemon: UnixStream, with_pipe: bool) -> Self {
        let abort = Abort::default();
        let over = std::sync::Arc::new(AtomicBool::new(false));
        let (woken, wake) = with_pipe.then(|| std::io::pipe().unwrap()).unzip();
        let (done, ended) = mpsc::channel();
        let (a, o) = (abort.clone(), over.clone());
        std::thread::spawn(move || {
            watch_hangup(&daemon, woken.as_ref(), &o, &a);
            let _ = done.send(());
        });
        Self {
            abort,
            over,
            wake,
            ended,
        }
    }

    fn ends_by(&self, bound: Duration) {
        self.ended.recv_timeout(bound).expect("the watch ended");
    }

    fn still_running(&self, d: Duration) {
        assert!(matches!(
            self.ended.recv_timeout(d),
            Err(RecvTimeoutError::Timeout)
        ));
    }
}

#[test]
fn closing_the_wake_ends_the_watch_at_once_with_no_hangup() {
    let (_cli, daemon) = UnixStream::pair().unwrap();
    let mut w = Watch::on(daemon, true);
    w.still_running(WATCH_POLL / 10);
    let t = Instant::now();
    w.wake.take();
    w.ends_by(STUCK);
    assert!(t.elapsed() < WATCH_POLL, "{:?}", t.elapsed());
    assert!(!w.abort.raised());
}

#[test]
fn with_no_pipe_the_watch_ends_within_a_poll_of_the_request_being_over() {
    let (_cli, daemon) = UnixStream::pair().unwrap();
    let w = Watch::on(daemon, false);
    w.still_running(WATCH_POLL / 10);
    w.over.store(true, Ordering::SeqCst);
    w.ends_by(STUCK);
    assert!(!w.abort.raised());
}

#[test]
fn a_quiet_cli_is_watched_across_many_polls_with_no_hangup() {
    let (_cli, daemon) = UnixStream::pair().unwrap();
    let w = Watch::on(daemon, true);
    w.still_running(WATCH_POLL * 2 + WATCH_POLL / 2);
    assert!(!w.abort.raised());
}

#[test]
fn a_hangup_is_seen_with_or_without_a_pipe() {
    for with_pipe in [true, false] {
        let (cli, daemon) = UnixStream::pair().unwrap();
        let w = Watch::on(daemon, with_pipe);
        w.still_running(WATCH_POLL / 10);
        let t = Instant::now();
        drop(cli);
        w.ends_by(STUCK);
        // Woken by it, not by the poll's timeout.
        assert!(t.elapsed() < WATCH_POLL, "{:?}", t.elapsed());
        assert_eq!(w.abort.why().as_deref(), Some(HANGUP));
    }
}

#[test]
fn the_cli_gone_before_the_watch_starts_is_a_hangup() {
    let (cli, daemon) = UnixStream::pair().unwrap();
    drop(cli);
    let w = Watch::on(daemon, true);
    w.ends_by(STUCK);
    assert_eq!(w.abort.why().as_deref(), Some(HANGUP));
}

#[test]
fn bytes_from_the_cli_are_no_hangup_and_its_going_afterwards_is() {
    let (mut cli, daemon) = UnixStream::pair().unwrap();
    let w = Watch::on(daemon, true);
    // More than one look's worth, so the watch goes round for them.
    cli.write_all(&[7u8; 1000]).unwrap();
    w.still_running(WATCH_POLL / 2);
    assert!(!w.abort.raised());
    // Gone with bytes unread: those are read past, and the end still seen.
    cli.write_all(&[7u8; 1000]).unwrap();
    drop(cli);
    w.ends_by(STUCK);
    assert_eq!(w.abort.why().as_deref(), Some(HANGUP));
}

#[test]
fn a_hangup_once_the_request_is_over_raises_nothing() {
    // Over already: the watch does not look.
    let (cli, daemon) = UnixStream::pair().unwrap();
    drop(cli);
    let (abort, over) = (Abort::default(), AtomicBool::new(true));
    watch_hangup(&daemon, None, &over, &abort);
    assert!(!abort.raised());

    // Woken, and the CLI gone too: the wake is looked at first.
    let (cli, daemon) = UnixStream::pair().unwrap();
    drop(cli);
    let (woken, wake) = std::io::pipe().unwrap();
    drop(wake);
    let (abort, over) = (Abort::default(), AtomicBool::new(false));
    watch_hangup(&daemon, Some(&woken), &over, &abort);
    assert!(!abort.raised());
}

#[test]
fn what_poll_says_is_read_correctly() {
    let (mut cli, daemon) = UnixStream::pair().unwrap();
    // Nothing said, an error, a dead descriptor, a hangup with nothing left.
    assert!(!hung_up(&daemon, 0));
    assert!(hung_up(&daemon, libc::POLLERR));
    assert!(hung_up(&daemon, libc::POLLNVAL));
    assert!(hung_up(&daemon, libc::POLLHUP));
    // Readable but nothing there: must come back, and is no hangup.
    let quiet = daemon.try_clone().unwrap();
    assert!(!within(STUCK, move || hung_up(&quiet, libc::POLLIN)));
    // Readable, and bytes: no hangup, even with the hangup flag beside them.
    cli.write_all(b"xx").unwrap();
    assert!(!hung_up(&daemon, libc::POLLIN | libc::POLLHUP));
    // Those were taken, and the socket stays blocking for what goes out.
    let flags = unsafe { libc::fcntl(daemon.as_raw_fd(), libc::F_GETFL) };
    assert_eq!(flags & libc::O_NONBLOCK, 0);
    drop(cli);
    assert!(hung_up(&daemon, libc::POLLIN));
}
