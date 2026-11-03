use super::*;

fn f(rel: &str, size: u64, mtime_ns: i64, mode: u32) -> Entry {
    Entry {
        rel: rel.into(),
        kind: EntryKind::File,
        size,
        mtime_ns,
        mode,
        link: None,
    }
}
fn d(rel: &str, mode: u32) -> Entry {
    Entry {
        rel: rel.into(),
        kind: EntryKind::Dir,
        size: 0,
        mtime_ns: 0,
        mode,
        link: None,
    }
}
fn l(rel: &str, to: &str, mode: u32) -> Entry {
    Entry {
        rel: rel.into(),
        kind: EntryKind::Symlink,
        size: 0,
        mtime_ns: 0,
        mode,
        link: Some(to.into()),
    }
}
fn m(mut entries: Vec<Entry>) -> Manifest {
    entries.sort();
    Manifest {
        entries,
        temps: vec![],
    }
}
fn lines(it: &Itemized) -> Vec<String> {
    it.changes
        .iter()
        .map(|c| {
            let k = match c.kind {
                ChangeKind::Create => '+',
                ChangeKind::Update => '~',
                ChangeKind::Attr => 'a',
                ChangeKind::Delete => '-',
            };
            format!("{k} {}{}", c.path, if c.is_dir { "/" } else { "" })
        })
        .collect()
}

#[test]
fn identical_trees_have_no_changes() {
    let tree = m(vec![
        d("a", 0o755),
        f("a/x", 3, 10, 0o644),
        l("ln", "a/x", 0o755),
    ]);
    let c = diff(&tree, &tree.clone());
    assert!(c.is_empty() && c.only_kept_dirs());
    assert!(itemize(&c, &tree).changes.is_empty());
    assert!(diff(&Manifest::default(), &Manifest::default()).is_empty());
}

#[test]
fn everything_missing_is_created_parents_first() {
    let src = m(vec![
        d("a", 0o755),
        d("a/b", 0o700),
        f("a/b/z", 1, 5, 0o600),
        f("a/y", 2, 5, 0o644),
        f("top", 3, 5, 0o644),
    ]);
    let c = diff(&src, &Manifest::default());
    let dirs: Vec<&str> = c.mkdir.iter().map(|e| e.rel.as_str()).collect();
    assert_eq!(dirs, ["a", "a/b"]);
    let files: Vec<&str> = c.send.iter().map(|e| e.rel.as_str()).collect();
    assert_eq!(files, ["a/b/z", "a/y", "top"]);
    assert!(c.delete.is_empty() && c.replace.is_empty());
    assert_eq!(c.totals(), (3, 6));
    let it = itemize(&c, &Manifest::default());
    assert_eq!(lines(&it), ["+ a/", "+ a/b/", "+ a/b/z", "+ a/y", "+ top"]);
    assert_eq!((it.files_changed(), it.deleted()), (3, 0));
}

#[test]
fn a_change_of_size_mtime_or_mode_sends_the_file() {
    let tgt = m(vec![
        f("size", 1, 5, 0o644),
        f("mtime", 1, 5, 0o644),
        f("mode", 1, 5, 0o644),
        f("same", 1, 5, 0o644),
    ]);
    let src = m(vec![
        f("size", 2, 5, 0o644),
        f("mtime", 1, 6, 0o644),
        f("mode", 1, 5, 0o755),
        f("same", 1, 5, 0o644),
    ]);
    let c = diff(&src, &tgt);
    let files: Vec<&str> = c.send.iter().map(|e| e.rel.as_str()).collect();
    assert_eq!(files, ["mode", "mtime", "size"]);
    let it = itemize(&c, &tgt);
    assert_eq!(lines(&it), ["a mode", "~ mtime", "~ size"]);
    // A mode-only change replaces no bytes anybody wrote: not "touched".
    assert_eq!(it.touched().collect::<Vec<_>>(), ["mtime", "size"]);
    assert_eq!(it.files_changed(), 2);
}

#[test]
fn a_directory_changes_only_by_mode() {
    let c = diff(&m(vec![d("a", 0o700)]), &m(vec![d("a", 0o755)]));
    assert_eq!(c.mkdir, [d("a", 0o700)]);
    assert_eq!(lines(&itemize(&c, &m(vec![d("a", 0o755)]))), ["a a/"]);
    assert!(
        itemize(&c, &m(vec![d("a", 0o755)])).is_empty(),
        "a directory's mode alone is not a change to show"
    );
}

#[test]
fn a_symlink_changes_by_target_never_by_mode() {
    let tgt = m(vec![l("a", "x", 0o777), l("b", "x", 0o777)]);
    let src = m(vec![l("a", "y", 0o755), l("b", "x", 0o755)]);
    let c = diff(&src, &tgt);
    assert_eq!(c.send, [l("a", "y", 0o755)]);
    assert_eq!(lines(&itemize(&c, &tgt)), ["~ a"]);
}

#[test]
fn kind_changes_remove_the_old_entry_first() {
    let tgt = m(vec![
        f("f2l", 1, 1, 0o644),
        f("f2d", 1, 1, 0o644),
        d("d2f", 0o755),
        f("d2f/inner", 1, 1, 0o644),
        l("l2d", "x", 0o777),
    ]);
    let src = m(vec![
        l("f2l", "t", 0o777),
        d("f2d", 0o755),
        f("f2d/new", 1, 1, 0o644),
        f("d2f", 9, 9, 0o644),
        d("l2d", 0o755),
    ]);
    let c = diff(&src, &tgt);
    assert_eq!(
        c.replace,
        [
            ("d2f".to_string(), EntryKind::Dir),
            ("f2d".to_string(), EntryKind::File),
            ("f2l".to_string(), EntryKind::File),
            ("l2d".to_string(), EntryKind::Symlink),
        ]
    );
    assert_eq!(c.delete, [("d2f/inner".to_string(), false)]);
    let it = itemize(&c, &tgt);
    assert_eq!(
        lines(&it),
        [
            "- d2f/",
            "+ d2f",
            "- d2f/inner",
            "- f2d",
            "+ f2d/",
            "+ f2d/new",
            "- f2l",
            "+ f2l",
            "- l2d",
            "+ l2d/"
        ]
    );
}

#[test]
fn deletions_go_children_first() {
    let tgt = m(vec![
        d("a", 0o755),
        d("a/b", 0o755),
        f("a/b/c", 1, 1, 0o644),
        f("a/x", 1, 1, 0o644),
        f("z", 1, 1, 0o644),
    ]);
    let c = diff(&Manifest::default(), &tgt);
    assert_eq!(
        c.delete,
        [
            ("z".to_string(), false),
            ("a/x".to_string(), false),
            ("a/b/c".to_string(), false),
            ("a/b".to_string(), true),
            ("a".to_string(), true),
        ]
    );
    let it = itemize(&c, &tgt);
    assert_eq!(it.deleted(), 3);
    assert!(!c.only_kept_dirs());
    let kept = ChangeList {
        delete: vec![("a".into(), true)],
        ..Default::default()
    };
    assert!(kept.only_kept_dirs() && !kept.is_empty());
}

#[test]
fn a_single_file_unit_is_its_root_entry() {
    let tgt = m(vec![f("", 1, 1, 0o644)]);
    let c = diff(&m(vec![f("", 2, 2, 0o644)]), &tgt);
    assert_eq!(lines(&itemize(&c, &tgt)), ["~ "]);
    let gone = diff(&Manifest::default(), &tgt);
    assert_eq!(gone.delete, [(String::new(), false)]);
}
