use super::*;
use crate::native::conn::Conn;
use std::time::Duration;

fn file(rel: &str, size: u64) -> Entry {
    Entry {
        rel: rel.into(),
        kind: EntryKind::File,
        size,
        mtime_ns: 7,
        mode: 0o644,
        link: None,
    }
}

#[test]
fn frames_round_trip_in_order() {
    let mut buf = Vec::new();
    let frames = [
        Frame::new(Kind::Ready, vec![]),
        Frame::new(Kind::Chunk, vec![7; 70_000]),
        Frame::json(Kind::Refused, &"no lease").unwrap(),
        Frame::new(Kind::Done, vec![]),
    ];
    for f in &frames {
        write(&mut buf, f).unwrap();
    }
    assert_eq!(&buf[..5], &[0, 0, 0, 0, Kind::Ready as u8]);
    let mut r = &buf[..];
    for f in &frames {
        assert_eq!(&read(&mut r).unwrap(), f);
    }
    let err = read(&mut r).unwrap_err().to_string();
    assert!(err.contains("closed the data connection"), "{err}");
}

#[test]
fn oversized_unknown_and_truncated_frames_are_refused() {
    let mut big = ((MAX_PAYLOAD + 1) as u32).to_be_bytes().to_vec();
    big.push(Kind::Chunk as u8);
    let err = read(&mut &big[..]).unwrap_err().to_string();
    assert!(err.contains("larger than the limit"), "{err}");

    let unknown = [0, 0, 0, 0, 99];
    let err = read(&mut &unknown[..]).unwrap_err().to_string();
    assert!(err.contains("unknown frame kind 99"), "{err}");

    let cut = [0, 0, 0, 9, Kind::Chunk as u8, 1, 2];
    let err = read(&mut &cut[..]).unwrap_err().to_string();
    assert!(err.contains("closed"), "{err}");
}

#[test]
fn expect_names_the_peers_reason() {
    let fail = Frame::json(Kind::Fail, &"disk full").unwrap();
    assert_eq!(
        fail.expect(Kind::Ack).unwrap_err().to_string(),
        "transfer: disk full"
    );
    let refused = Frame::json(Kind::Refused, &"not at the head").unwrap();
    assert!(
        refused
            .expect(Kind::Ready)
            .unwrap_err()
            .to_string()
            .contains("not at the head")
    );
    let other = Frame::new(Kind::Done, vec![])
        .expect(Kind::Ack)
        .unwrap_err()
        .to_string();
    assert!(other.contains("expected a Ack frame, got Done"), "{other}");
    assert!(Frame::new(Kind::Ack, vec![]).expect(Kind::Ack).is_ok());
}

#[test]
fn a_batch_carries_its_entries_and_exactly_their_bytes() {
    let link = Entry {
        rel: "l".into(),
        kind: EntryKind::Symlink,
        size: 0,
        mtime_ns: 0,
        mode: 0o777,
        link: Some("a".into()),
    };
    let entries = vec![file("a", 3), link, file("b", 2)];
    let f = batch(&entries, b"abcde").unwrap();
    let (got, bytes) = unbatch(&f).unwrap();
    assert_eq!((got, bytes), (entries.clone(), &b"abcde"[..]));

    let short = batch(&entries, b"abcd").unwrap();
    assert!(
        unbatch(&short)
            .unwrap_err()
            .to_string()
            .contains("5 bytes, it carries 4")
    );
    let mut garbled = f.clone();
    garbled.payload[0] = 0xff;
    assert!(
        unbatch(&garbled)
            .unwrap_err()
            .to_string()
            .contains("header past its end")
    );
    assert!(unbatch(&Frame::new(Kind::Batch, vec![1])).is_err());
}

#[test]
fn a_silent_peer_times_out_as_a_dead_connection() {
    let (mut a, _b) = Conn::pair().unwrap();
    a.set_timeout(Duration::from_millis(50)).unwrap();
    let t = std::time::Instant::now();
    let err = a.recv().unwrap_err().to_string();
    assert!(err.contains("connection is dead"), "{err}");
    assert!(t.elapsed() < Duration::from_secs(5));
}

#[test]
fn shutting_down_from_another_thread_unblocks_a_reader() {
    let (mut a, _b) = Conn::pair().unwrap();
    let closer = a.closer().unwrap();
    let t = std::time::Instant::now();
    std::thread::scope(|s| {
        s.spawn(|| {
            std::thread::sleep(Duration::from_millis(50));
            closer.shutdown();
        });
        assert!(a.recv().is_err());
    });
    assert!(t.elapsed() < Duration::from_secs(5));
}
