//! How a unit's sends are split into work: big files alone and first, the
//! rest batched; a repo's refs in a round of their own, last.

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
