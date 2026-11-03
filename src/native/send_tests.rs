use super::*;
use crate::native::manifest::EntryKind;
use crate::testutil::write;

/// A receiver that acks everything and remembers each frame's kind and size.
fn acker(mut c: Conn) -> Vec<(Kind, usize)> {
    let mut seen = Vec::new();
    while let Ok(f) = c.recv() {
        seen.push((f.kind, f.payload.len()));
        if c.send(&Frame::new(Kind::Ack, vec![])).is_err() {
            break;
        }
    }
    seen
}

#[test]
fn a_large_file_streams_in_chunks_and_ends_once_acked() {
    let t = tempfile::tempdir().unwrap();
    let bytes: Vec<u8> = (0..(CHUNK * 3 + 17)).map(|i| i as u8).collect();
    std::fs::write(t.path().join("big"), &bytes).unwrap();
    let src = Source::open(t.path(), false, None).unwrap();
    let (mut leaf, home) = Conn::pair().unwrap();
    let counted = AtomicU64::new(0);
    let seen = std::thread::scope(|s| {
        let h = s.spawn(|| acker(home));
        let sent = file(&mut Window::counting(&mut leaf, &counted), &src, "big").unwrap();
        assert!(matches!(&sent, Sent::Whole(e) if e.size == bytes.len() as u64));
        drop(leaf);
        h.join().unwrap()
    });
    let kinds: Vec<Kind> = seen.iter().map(|(k, _)| *k).collect();
    assert_eq!(
        kinds,
        [
            Kind::FileStart,
            Kind::Chunk,
            Kind::Chunk,
            Kind::Chunk,
            Kind::Chunk,
            Kind::FileEnd
        ]
    );
    assert!(seen.iter().all(|(_, n)| *n <= CHUNK));
    assert_eq!(
        counted.load(Ordering::Relaxed),
        bytes.len() as u64,
        "every byte acked"
    );
}

#[test]
fn a_file_gone_before_it_is_sent_is_not_sent() {
    let t = tempfile::tempdir().unwrap();
    let src = Source::open(t.path(), false, None).unwrap();
    let (mut leaf, _home) = Conn::pair().unwrap();
    let sent = file(&mut Window::new(&mut leaf), &src, "gone").unwrap();
    assert_eq!(sent, Sent::Changed("gone".into()));
}

#[test]
fn a_batch_leaves_out_what_changed_and_is_none_when_nothing_is_left() {
    let t = tempfile::tempdir().unwrap();
    write(&t.path().join("a"), "aa");
    std::os::unix::fs::symlink("a", t.path().join("l")).unwrap();
    let src = Source::open(t.path(), false, None).unwrap();
    let (f, sent) = batch(&src, &["a".into(), "gone".into(), "l".into()]).unwrap();
    let f = f.unwrap();
    let (entries, bytes) = frame::unbatch(&f).unwrap();
    assert_eq!(
        entries.iter().map(|e| e.rel.as_str()).collect::<Vec<_>>(),
        ["a", "l"]
    );
    assert_eq!(entries[1].kind, EntryKind::Symlink);
    assert_eq!(bytes, b"aa");
    assert_eq!(sent[1], Sent::Changed("gone".into()));
    let (none, _) = batch(&src, &["gone".into()]).unwrap();
    assert!(none.is_none());
}

#[test]
fn the_window_holds_at_most_its_size_unacked_and_a_fail_says_why() {
    let (mut leaf, mut home) = Conn::pair().unwrap();
    std::thread::scope(|s| {
        let h = s.spawn(move || {
            // Read WINDOW + 1 frames before answering any: the sender must
            // have stopped at WINDOW and waited.
            let mut n = 0;
            for _ in 0..WINDOW {
                home.recv().unwrap();
                n += 1;
            }
            for _ in 0..WINDOW {
                home.send(&Frame::new(Kind::Ack, vec![])).unwrap();
            }
            home.recv().unwrap();
            n += 1;
            home.send(&Frame::json(Kind::Fail, &"disk full").unwrap())
                .unwrap();
            n
        });
        let mut w = Window::new(&mut leaf);
        for _ in 0..=WINDOW {
            w.send(&Frame::new(Kind::Chunk, vec![0; 10]), 10).unwrap();
        }
        let err = w.drain().unwrap_err().to_string();
        assert!(err.contains("disk full"), "{err}");
        assert_eq!(h.join().unwrap(), WINDOW + 1);
    });
}
