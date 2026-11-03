//! How a unit's sends are split into work: big files alone and first, the
//! rest batched; a repo's refs in a round of their own, last.

/// A file so named panics the worker that takes it: the only way to make one
/// panic, and only in tests. Names are per test, so tests do not meet.
pub(super) const PANIC_PREFIX: &str = "panic-me-";

pub(super) fn forced_panic(item: &super::Item) {
    let hit = |rel: &String| {
        rel.rsplit('/')
            .next()
            .unwrap_or(rel)
            .starts_with(PANIC_PREFIX)
    };
    match item {
        super::Item::Batch(rels) if rels.iter().any(hit) => panic!("forced, in a worker"),
        super::Item::Big(rel) if hit(rel) => panic!("forced, in a worker"),
        _ => {}
    }
}

#[test]
fn a_batch_closes_at_its_paths_however_few_or_small_the_files() {
    use super::{Item, items};
    let f = |i: usize| crate::native::Entry {
        rel: format!("{i:04}-{}", "n".repeat(250)),
        kind: crate::native::EntryKind::File,
        size: 1,
        mtime_ns: 0,
        mode: 0o644,
        link: None,
    };
    let send: Vec<_> = (0..4000).map(f).collect();
    let batches = items(&send);
    assert!(batches.len() >= 2, "{}", batches.len());
    for b in &batches {
        let Item::Batch(rels) = b else { panic!() };
        let j = crate::native::frame::Frame::json(crate::native::frame::Kind::Get, rels).unwrap();
        assert!(j.payload.len() < crate::native::frame::MAX_PAYLOAD);
    }
    let n: usize = batches
        .iter()
        .map(|b| match b {
            Item::Batch(r) => r.len(),
            Item::Big(_) => 1,
        })
        .sum();
    assert_eq!(n, 4000, "none lost");
}

#[test]
fn work_items_put_big_files_first_and_batch_the_rest() {
    use super::{Item, items};
    let f = |rel: &str, size| crate::native::Entry {
        rel: rel.into(),
        kind: crate::native::EntryKind::File,
        size,
        mtime_ns: 0,
        mode: 0o644,
        link: None,
    };
    let send = vec![
        f("a", 10),
        f("b", 5 << 20),
        f("c", 70_000),
        f("d", 20),
        f("e", 1 << 30),
    ];
    assert_eq!(
        items(&send),
        [
            Item::Big("e".into()),
            Item::Big("b".into()),
            Item::Big("c".into()),
            Item::Batch(vec!["a".into(), "d".into()])
        ]
    );
    let many: Vec<_> = (0..5000).map(|i| f(&format!("f{i:04}"), 1)).collect();
    let it = items(&many);
    assert_eq!(it.len(), 3, "batches close at their entry limit");
    let lots: Vec<_> = (0..40).map(|i| f(&format!("m{i:02}"), 60_000)).collect();
    assert!(items(&lots).len() >= 2, "batches close at their byte limit");
}

#[test]
fn a_repos_refs_go_in_a_round_after_everything_else() {
    use super::{is_ref, rounds};
    for r in [
        ".git/HEAD",
        ".git/packed-refs",
        ".git/refs/heads/main",
        "sub/.git/refs/tags/v1",
    ] {
        assert!(is_ref(r), "{r}");
    }
    for r in [
        ".git/objects/ab/cd",
        ".git/index",
        "HEAD",
        "refs/x",
        ".git/HEAD.lock",
        "src/.git-thing",
    ] {
        assert!(!is_ref(r), "{r}");
    }
    let f = |rel: &str| crate::native::Entry {
        rel: rel.into(),
        kind: crate::native::EntryKind::File,
        size: 1,
        mtime_ns: 0,
        mode: 0o644,
        link: None,
    };
    let [first, last] = rounds(&[
        f(".git/HEAD"),
        f(".git/objects/ab/cd"),
        f(".git/refs/heads/main"),
        f("README"),
    ]);
    use super::Item;
    assert_eq!(
        first,
        [Item::Batch(vec![
            ".git/objects/ab/cd".into(),
            "README".into()
        ])]
    );
    assert_eq!(
        last,
        [Item::Batch(vec![
            ".git/HEAD".into(),
            ".git/refs/heads/main".into()
        ])]
    );
}

mod a_pull_takes_only_what_it_asked_for {
    use super::super::*;
    use crate::native::tests_fixture::bounded;
    use crate::testutil::write;
    use std::path::Path;
    use std::time::Duration;

    fn file(rel: &str, bytes: &[u8]) -> Entry {
        Entry {
            rel: rel.into(),
            kind: EntryKind::File,
            size: bytes.len() as u64,
            mtime_ns: 1,
            mode: 0o644,
            link: None,
        }
    }

    /// Pull `asked` into `root`, `home` answering the one `Get`; the pull's
    /// result.
    fn pull_from(
        root: &Path,
        asked: Vec<&str>,
        home: impl FnOnce(&mut Conn) + Send + 'static,
    ) -> Result<()> {
        let (mut leaf, mut far) = Conn::pair().unwrap();
        let t = std::thread::spawn(move || {
            far.recv().unwrap().expect(Kind::Get).unwrap();
            home(&mut far);
            far
        });
        let rx = Receiver::open(root, false, None, None).unwrap();
        let sh = Shared::new(vec![Item::Batch(
            asked.into_iter().map(String::from).collect(),
        )]);
        let r = pull(&mut leaf, &rx, &sh, &Abort::default());
        drop(leaf);
        drop(t.join());
        r
    }

    /// p09: home answers `Get ["asked"]` with the leaf's only copy of something.
    #[test]
    fn a_batch_entry_not_asked_for_is_refused_and_not_written() {
        bounded(Duration::from_secs(30), || {
            let t = tempfile::tempdir().unwrap();
            write(&t.path().join("leaf-only.txt"), "leaf-only work");
            let r = pull_from(t.path(), vec!["asked"], |c| {
                let e = [file("asked", b"a"), file("leaf-only.txt", b"home's")];
                let _ = c.send(&frame::batch(&e, b"ahome's").unwrap());
            });
            let e = r.unwrap_err().to_string();
            assert!(e.contains("not asked for"), "{e}");
            let got = std::fs::read(t.path().join("leaf-only.txt")).unwrap();
            assert_eq!(got, b"leaf-only work");
            assert!(
                !t.path().join("asked").exists(),
                "the batch is refused whole"
            );
        });
    }

    #[test]
    fn a_large_file_not_asked_for_is_refused_and_not_written() {
        bounded(Duration::from_secs(30), || {
            let t = tempfile::tempdir().unwrap();
            write(&t.path().join("leaf-only.txt"), "leaf-only work");
            let r = pull_from(t.path(), vec!["asked"], |c| {
                let f = Frame::json(Kind::FileStart, &file("leaf-only.txt", b"home's")).unwrap();
                let _ = c.send(&f);
            });
            assert!(r.unwrap_err().to_string().contains("not asked for"));
            let got = std::fs::read(t.path().join("leaf-only.txt")).unwrap();
            assert_eq!(got, b"leaf-only work");
        });
    }

    #[test]
    fn what_was_asked_for_is_written() {
        bounded(Duration::from_secs(30), || {
            let t = tempfile::tempdir().unwrap();
            let r = pull_from(t.path(), vec!["asked"], |c| {
                c.send(&frame::batch(&[file("asked", b"a")], b"a").unwrap())
                    .unwrap();
                c.recv().unwrap().expect(Kind::Ack).unwrap();
                c.send(&Frame::new(Kind::Done, vec![])).unwrap();
            });
            r.unwrap();
            assert_eq!(std::fs::read(t.path().join("asked")).unwrap(), b"a");
        });
    }
}
