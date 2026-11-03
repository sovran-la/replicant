use super::*;
use crate::native::manifest::Manifest;
use crate::testutil::write;
use std::cell::Cell;
use std::path::Path;

fn file(rel: &str, size: u64, mtime_ns: i64) -> Entry {
    Entry {
        rel: rel.into(),
        kind: EntryKind::File,
        size,
        mtime_ns,
        mode: 0o644,
        link: None,
    }
}
fn dir(rel: &str, mode: u32) -> Entry {
    Entry {
        rel: rel.into(),
        kind: EntryKind::Dir,
        size: 0,
        mtime_ns: 0,
        mode,
        link: None,
    }
}
fn served(root: &Path, backup: Option<PathBuf>, policy: &[String]) -> Served {
    Served {
        host: "leaf".into(),
        direction: Direction::Push,
        root: root.to_path_buf(),
        is_file: false,
        policy: Excludes::for_root(policy).unwrap(),
        backup,
    }
}

/// Run home's side on a thread; `leaf` drives the other end. Home's result.
fn push(
    s: &Served,
    still: &(dyn Fn() -> Result<()> + Sync),
    leaf: impl FnOnce(&mut Conn),
) -> Result<()> {
    let (mut l, mut h) = Conn::pair().unwrap();
    std::thread::scope(|sc| {
        let home = sc.spawn(move || answer(&mut h, s, still));
        leaf(&mut l);
        drop(l);
        home.join().unwrap()
    })
}

fn acked(c: &mut Conn, f: Frame) -> Frame {
    c.send(&f).unwrap();
    c.recv().unwrap().expect(Kind::Ack).unwrap()
}

#[test]
fn a_whole_push_lands_with_every_frame_acked_in_order() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("unit");
    write(&root.join("old.txt"), "to delete");
    write(&root.join("kept/.env"), "not synced, never deleted");
    write(&root.join("kept/gone"), "x");
    write(&root.join("was-file"), "becomes a dir");
    write(&root.join(".replicant-tmp-x"), "dead run");
    let run = t.path().join("backup/run-1");
    let s = served(&root, Some(run.clone()), &[]);
    let ok = || Ok(());
    let big: Vec<u8> = (0..300_000u32).map(|i| i as u8).collect();
    push(&s, &ok, |c| {
        acked(c, Frame::json(Kind::Clean, &[".replicant-tmp-x"]).unwrap());
        let r = [Replace {
            rel: "was-file".into(),
            kind: EntryKind::File,
        }];
        acked(c, Frame::json(Kind::Replace, &r).unwrap());
        acked(
            c,
            Frame::json(Kind::Mkdirs, &[dir("was-file", 0o755), dir("d", 0o700)]).unwrap(),
        );
        let small = [file("d/a", 3, 11), file("b", 2, 12)];
        acked(c, frame::batch(&small, b"aaabb").unwrap());
        acked(
            c,
            Frame::json(Kind::FileStart, &file("d/big", big.len() as u64, 13)).unwrap(),
        );
        for chunk in big.chunks(100_000) {
            acked(c, Frame::new(Kind::Chunk, chunk.to_vec()));
        }
        acked(c, Frame::new(Kind::FileEnd, vec![]));
        let del = [
            ("old.txt".to_string(), false),
            ("kept/gone".into(), false),
            ("kept".into(), true),
        ];
        let kept: Vec<String> = acked(c, Frame::json(Kind::Deletes, &del).unwrap())
            .parse()
            .unwrap();
        assert_eq!(kept, ["kept"]);
        acked(
            c,
            Frame::json(Kind::DirModes, &[dir("d", 0o700), dir("was-file", 0o755)]).unwrap(),
        );
        c.send(&Frame::new(Kind::Done, vec![])).unwrap();
    })
    .unwrap();

    let none = Excludes::for_root(&[]).unwrap();
    let m = Manifest::walk(&root, &none, false).unwrap();
    let rels: Vec<&str> = m.entries.iter().map(|e| e.rel.as_str()).collect();
    assert_eq!(
        rels,
        ["b", "d", "d/a", "d/big", "kept", "kept/.env", "was-file"]
    );
    assert_eq!(fs_read(&root.join("d/big")), big);
    assert_eq!(m.get("d/big").unwrap().mtime_ns, 13);
    assert_eq!(m.get("d").unwrap().mode, 0o700);
    assert!(m.temps.is_empty());
    for (rel, was) in [
        ("old.txt", "to delete"),
        ("kept/gone", "x"),
        ("was-file", "becomes a dir"),
    ] {
        assert_eq!(
            String::from_utf8(fs_read(&run.join(rel))).unwrap(),
            was,
            "{rel} backed up"
        );
    }
}

fn fs_read(p: &Path) -> Vec<u8> {
    std::fs::read(p).unwrap()
}

#[test]
fn a_lost_lease_stops_home_before_the_next_write() {
    let t = tempfile::tempdir().unwrap();
    let s = served(t.path(), None, &[]);
    let n = std::sync::atomic::AtomicUsize::new(0);
    let still = || match n.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
        0 => Ok(()),
        _ => Err(Error::Refused("lease 3 is not queued".into())),
    };
    let err = push(&s, &still, |c| {
        acked(c, frame::batch(&[file("first", 1, 1)], b"1").unwrap());
        c.send(&frame::batch(&[file("second", 1, 1)], b"2").unwrap())
            .unwrap();
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("not queued"), "{err}");
    assert!(t.path().join("first").exists());
    assert!(!t.path().join("second").exists());
}

#[test]
fn frames_out_of_place_or_against_policy_end_the_push() {
    let t = tempfile::tempdir().unwrap();
    let ok = || Ok(());
    let s = served(t.path(), None, &["secrets".into()]);
    let cases: Vec<(Frame, &str)> = vec![
        (Frame::new(Kind::Chunk, vec![1]), "no file begun"),
        (Frame::new(Kind::FileEnd, vec![]), "no file begun"),
        (
            Frame::json(Kind::Get, &Vec::<Entry>::new()).unwrap(),
            "no place in a push",
        ),
        (
            frame::batch(&[file("secrets/k", 1, 1)], b"k").unwrap(),
            "excluded on home",
        ),
        (
            frame::batch(&[dir("d", 0o755)], b"").unwrap(),
            "a directory in a batch",
        ),
        (
            Frame::json(Kind::Clean, &["not-temp"]).unwrap(),
            "not a temporary name",
        ),
    ];
    for (f, why) in cases {
        let err = push(&s, &ok, |c| c.send(&f).unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains(why), "{why}: {err}");
    }
    assert!(!t.path().join("secrets").exists());
}

#[test]
fn a_discarded_file_leaves_its_target_as_it_was() {
    let t = tempfile::tempdir().unwrap();
    write(&t.path().join("f"), "old");
    let s = served(t.path(), None, &[]);
    let ok = || Ok(());
    let reached = Cell::new(false);
    push(&s, &ok, |c| {
        acked(c, Frame::json(Kind::FileStart, &file("f", 6, 1)).unwrap());
        acked(c, Frame::new(Kind::Chunk, b"new".to_vec()));
        acked(c, Frame::new(Kind::FileDiscard, vec![]));
        c.send(&Frame::new(Kind::Done, vec![])).unwrap();
        reached.set(true);
    })
    .unwrap();
    assert!(reached.get());
    assert_eq!(
        String::from_utf8(fs_read(&t.path().join("f"))).unwrap(),
        "old"
    );
    assert_eq!(
        std::fs::read_dir(t.path()).unwrap().count(),
        1,
        "no temporary left"
    );
}
