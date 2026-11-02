//! The local socket's stream: frames round-trip, an answer reads past any
//! number of progress frames sent in one burst, an old daemon's refusal
//! still reads, and frames reach the CLI in order before the answer.

use super::*;
use crate::marker::Direction;
use crate::plan::apply::UnitOutcome;

fn started(i: usize) -> ProgressEvent {
    ProgressEvent::UnitStarted {
        rel: format!("work/{i}"),
        action: "push".into(),
        direction: Some(Direction::Push),
        index: i,
        total: 3,
    }
}

fn done(outcome: UnitOutcome) -> ProgressEvent {
    ProgressEvent::UnitDone {
        rel: "work/a b".into(),
        action: "adopt".into(),
        direction: None,
        files_changed: 220,
        deleted: 3,
        elapsed_ms: 1_400,
        push: Some("20261102T010203Z-1".into()),
        outcome,
    }
}

fn every_event() -> Vec<ProgressEvent> {
    vec![
        started(1),
        done(UnitOutcome::Applied),
        done(UnitOutcome::Interrupted {
            why: "the CLI hung up; rsync stopped".into(),
        }),
        done(UnitOutcome::Failed {
            why: "home unreachable\nsecond line".into(),
        }),
    ]
}

fn line(f: &LocalFrame) -> Vec<u8> {
    let mut v = serde_json::to_vec(f).unwrap();
    v.push(b'\n');
    v
}

#[test]
fn every_frame_round_trips() {
    for e in every_event() {
        let back: LocalFrame =
            serde_json::from_slice(&line(&LocalFrame::Progress(e.clone()))).unwrap();
        let LocalFrame::Progress(back) = back else {
            panic!("{back:?}")
        };
        assert_eq!(back, e);
    }
    let pong = LocalFrame::Ok(LocalReply::Pong {
        role: "leaf".into(),
        name: "birchbook".into(),
    });
    let back: LocalFrame = serde_json::from_slice(&line(&pong)).unwrap();
    assert!(matches!(back, LocalFrame::Ok(LocalReply::Pong { name, .. }) if name == "birchbook"));
    let back: LocalFrame = serde_json::from_slice(&line(&LocalFrame::Err("no".into()))).unwrap();
    assert!(matches!(back, LocalFrame::Err(e) if e == "no"));
}

#[test]
fn an_answer_is_spelled_as_before_so_an_old_daemon_s_refusal_reads() {
    let old: LocalAnswer = Err("the CLI speaks wire version 5 and this daemon 4".into());
    let j = serde_json::to_vec(&old).unwrap();
    assert_eq!(j, serde_json::to_vec(&LocalFrame::from(old)).unwrap());
    let mut r = std::io::Cursor::new([j, b"\n".to_vec()].concat());
    let err = read_answer(&mut r, &mut |_| panic!("no progress")).unwrap_err();
    assert!(err.to_string().contains("wire version 5"), "{err}");
}

#[test]
fn read_answer_hands_over_every_frame_of_a_burst_in_order() {
    let mut bytes = Vec::new();
    for e in every_event() {
        bytes.extend(line(&LocalFrame::Progress(e)));
    }
    bytes.extend(line(&LocalFrame::Ok(LocalReply::Pong {
        role: "leaf".into(),
        name: "x".into(),
    })));
    let mut seen = Vec::new();
    let mut r = std::io::BufReader::new(std::io::Cursor::new(bytes));
    let reply = read_answer(&mut r, &mut |e| seen.push(e)).unwrap();
    assert!(matches!(reply, LocalReply::Pong { .. }));
    assert_eq!(seen, every_event());

    // Cut off before the answer: an error, not a hang or a made-up reply.
    let mut cut = Vec::new();
    cut.extend(line(&LocalFrame::Progress(started(1))));
    let err = read_answer(&mut std::io::Cursor::new(cut), &mut |_| {}).unwrap_err();
    assert!(err.to_string().contains("closed the connection"), "{err}");
}

fn request(s: &mut UnixStream, version: u32) {
    send(
        s,
        &LocalCall {
            version,
            request: LocalRequest::Ping,
        },
    )
    .unwrap();
}

#[test]
fn serve_streams_progress_then_answers() {
    let (mut cli, daemon) = UnixStream::pair().unwrap();
    std::thread::scope(|s| {
        s.spawn(|| {
            serve(daemon, &|_, p| {
                p.emit(started(1));
                p.emit(started(2));
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
        assert_eq!(seen, vec![started(1), started(2)]);
    });
}

#[test]
fn another_version_is_refused_by_name() {
    let (mut cli, daemon) = UnixStream::pair().unwrap();
    request(&mut cli, 4);
    serve(daemon, &|_, _| panic!("not handled"));
    let err = read_answer(&mut BufReader::new(&cli), &mut |_| {})
        .unwrap_err()
        .to_string();
    assert!(err.contains("wire version 4 and this daemon 5"), "{err}");
}
