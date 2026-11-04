//! What a change list opens, and what gets its mode back (the re-review's m10).

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

fn dir_e(rel: &str, mode: u32) -> Entry {
    Entry {
        rel: rel.into(),
        kind: EntryKind::Dir,
        size: 0,
        mtime_ns: 0,
        mode,
        link: None,
    }
}

fn file_e(rel: &str, size: u64) -> Entry {
    f(rel, size, 1, 0o644)
}

fn side(entries: &[Entry]) -> Manifest {
    let mut entries = entries.to_vec();
    entries.sort();
    Manifest {
        entries,
        ..Default::default()
    }
}

fn opened(source: &[Entry], target: &[Entry]) -> Vec<String> {
    let c = diff(&side(source), &side(target));
    c.open.into_iter().map(|e| e.rel).collect()
}

const RO: u32 = 0o555;

#[test]
fn a_read_only_parent_of_each_kind_of_write_is_opened() {
    let ro = dir_e("ro", RO);
    let cases: [(&str, Vec<Entry>, Vec<Entry>); 5] = [
        ("send", vec![file_e("ro/f", 2)], vec![file_e("ro/f", 1)]),
        ("new", vec![file_e("ro/f", 2)], vec![]),
        ("delete", vec![], vec![file_e("ro/f", 1)]),
        ("mkdir", vec![dir_e("ro/d", 0o755)], vec![]),
        (
            "replace",
            vec![dir_e("ro/f", 0o755)],
            vec![file_e("ro/f", 1)],
        ),
    ];
    for (what, mut s, mut t) in cases {
        s.push(ro.clone());
        t.push(ro.clone());
        assert_eq!(opened(&s, &t), ["ro"], "{what}");
    }
}

#[test]
fn it_is_no_line_of_the_plan() {
    let ro = dir_e("ro", RO);
    let (s, t) = (
        side(&[ro.clone(), file_e("ro/f", 2)]),
        side(&[ro, file_e("ro/f", 1)]),
    );
    let it = itemize(&diff(&s, &t), &t);
    let paths: Vec<&str> = it.changes.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(paths, ["ro/f"]);
}

#[test]
fn it_takes_the_targets_mode() {
    let (s, t) = (
        side(&[dir_e("ro", RO), file_e("ro/f", 2)]),
        side(&[dir_e("ro", RO), file_e("ro/f", 1)]),
    );
    assert_eq!(diff(&s, &t).open, [dir_e("ro", RO)]);
}

#[test]
fn nothing_written_in_it_leaves_it_shut() {
    let ro = dir_e("ro", RO);
    let same = [ro.clone(), file_e("ro/f", 1)];
    assert!(opened(&same, &same).is_empty(), "no write");
    assert!(
        opened(
            &[ro.clone(), file_e("ro/f", 1), file_e("g", 2)],
            &[ro.clone(), file_e("ro/f", 1), file_e("g", 1)],
        )
        .is_empty(),
        "a write elsewhere"
    );
    assert!(
        opened(
            &[ro.clone(), dir_e("ro/d", 0o700)],
            &[ro, dir_e("ro/d", 0o755)],
        )
        .is_empty(),
        "changing a child's mode writes nothing in its parent"
    );
}

#[test]
fn only_the_direct_parent_is_opened() {
    let (a, b) = (dir_e("a", RO), dir_e("a/b", RO));
    assert_eq!(
        opened(
            &[a.clone(), b.clone(), file_e("a/b/f", 2)],
            &[a, b, file_e("a/b/f", 1)],
        ),
        ["a/b"]
    );
}

#[test]
fn a_writable_directory_needs_no_opening() {
    for mode in [0o755, 0o700, 0o200] {
        let d = dir_e("d", mode);
        assert!(
            opened(&[d.clone(), file_e("d/f", 2)], &[d, file_e("d/f", 1)]).is_empty(),
            "{mode:o}"
        );
    }
    // The owner's bit is the one that counts.
    let d = dir_e("d", 0o577);
    assert_eq!(
        opened(&[d.clone(), file_e("d/f", 2)], &[d, file_e("d/f", 1)]),
        ["d"]
    );
}

/// `mkdir` opens its own, with the source's mode to end on.
#[test]
fn a_directory_whose_mode_changes_anyway_is_not_opened_twice() {
    assert!(
        opened(
            &[dir_e("ro", 0o755), file_e("ro/f", 2)],
            &[dir_e("ro", RO), file_e("ro/f", 1)],
        )
        .is_empty()
    );
}

#[test]
fn a_top_level_path_has_no_parent_to_open() {
    assert!(opened(&[file_e("f", 2)], &[file_e("f", 1)]).is_empty());
}

/// A link is never written through, whatever it reads as.
#[test]
fn a_link_where_a_directory_might_be_is_not_opened() {
    let l = Entry {
        rel: "l".into(),
        kind: EntryKind::Symlink,
        size: 0,
        mtime_ns: 0,
        mode: RO,
        link: Some("elsewhere".into()),
    };
    assert!(opened(&[dir_e("l", 0o755), file_e("l/f", 1)], &[l]).is_empty());
}

#[test]
fn final_modes_are_deepest_first_and_leave_out_what_is_gone() {
    let c = ChangeList {
        mkdir: vec![dir_e("a", 0o755), dir_e("a/b", 0o755)],
        open: vec![
            dir_e("a/c", RO),
            dir_e("emptied", RO),
            dir_e("stays", RO),
            dir_e("taken", RO),
            dir_e("z", RO),
        ],
        replace: vec![("taken".into(), EntryKind::Dir)],
        delete: vec![("stays".into(), true), ("emptied".into(), true)],
        ..Default::default()
    };
    let rels: Vec<String> = c
        .final_modes(&["stays".to_string()])
        .into_iter()
        .map(|e| e.rel)
        .collect();
    assert_eq!(rels, ["z", "stays", "a/c", "a/b", "a"]);
}
