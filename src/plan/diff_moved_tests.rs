//! [`target_moved`]: what counts as a listed path having moved since the plan.

use super::*;
use crate::changes::{Change, ChangeKind, Itemized};
use crate::native::diff::ChangeList;
use crate::native::manifest::{Entry, EntryKind};

fn file(rel: &str) -> Entry {
    Entry {
        rel: rel.into(),
        kind: EntryKind::File,
        size: 3,
        mtime_ns: 7,
        mode: 0o644,
        link: None,
    }
}

fn manifest(entries: &[Entry]) -> Manifest {
    let mut entries = entries.to_vec();
    entries.sort();
    Manifest {
        entries,
        ..Default::default()
    }
}

/// A fresh dry run listing `listed`, with `target` as what it saw.
fn fresh(listed: &[(&str, ChangeKind)], target: Manifest) -> Diffed {
    Diffed {
        itemized: Itemized {
            changes: listed
                .iter()
                .map(|(p, kind)| Change {
                    kind: *kind,
                    path: (*p).into(),
                    is_dir: false,
                })
                .collect(),
            notes: vec![],
        },
        changes: ChangeList::default(),
        temps: vec![],
        target,
    }
}

fn moved(then: &[Entry], listed: &[(&str, ChangeKind)], now: &[Entry]) -> bool {
    target_moved(&manifest(then), &fresh(listed, manifest(now)))
}

const UPDATE: ChangeKind = ChangeKind::Update;

#[test]
fn a_listed_path_as_the_plan_saw_it_has_not_moved() {
    let f = file("f");
    assert!(!moved(
        std::slice::from_ref(&f),
        &[("f", UPDATE)],
        std::slice::from_ref(&f)
    ));
    assert!(!moved(&[], &[], &[]), "nothing listed");
}

#[test]
fn a_path_absent_both_times_has_not_moved() {
    assert!(!moved(&[], &[("new", ChangeKind::Create)], &[]));
}

#[test]
fn each_of_kind_size_mtime_and_link_is_a_move() {
    let then = file("f");
    type Edit = fn(&mut Entry);
    let changes: [(&str, Edit); 4] = [
        ("size", |e| e.size += 1),
        ("mtime", |e| e.mtime_ns += 1),
        ("kind", |e| e.kind = EntryKind::Symlink),
        ("link", |e| e.link = Some("elsewhere".into())),
    ];
    for (what, change) in changes {
        let mut now = then.clone();
        change(&mut now);
        assert!(
            moved(std::slice::from_ref(&then), &[("f", UPDATE)], &[now]),
            "{what}"
        );
    }
}

#[test]
fn a_mode_is_not_a_move() {
    let then = file("f");
    let now = Entry {
        mode: 0o600,
        ..then.clone()
    };
    assert!(!moved(&[then], &[("f", UPDATE)], &[now]));
}

#[test]
fn appearing_and_vanishing_are_moves() {
    assert!(moved(&[], &[("f", UPDATE)], &[file("f")]), "appeared");
    assert!(moved(&[file("f")], &[("f", UPDATE)], &[]), "vanished");
}

#[test]
fn only_listed_paths_count() {
    let then = file("other");
    let now = Entry {
        size: 99,
        ..then.clone()
    };
    assert!(!moved(
        &[file("f"), then],
        &[("f", UPDATE)],
        &[file("f"), now]
    ));
}

/// Any one of many is enough: not just the first looked at.
#[test]
fn a_move_late_in_the_list_counts() {
    let last = file("z");
    let now = Entry {
        size: 99,
        ..last.clone()
    };
    assert!(moved(
        &[file("a"), last],
        &[("a", UPDATE), ("z", ChangeKind::Delete)],
        &[file("a"), now]
    ));
}

/// A single-file unit's one entry is called `""`.
#[test]
fn a_single_files_root_entry_counts() {
    let then = file("");
    let now = Entry {
        size: 99,
        ..then.clone()
    };
    assert!(moved(std::slice::from_ref(&then), &[("", UPDATE)], &[now]));
    assert!(!moved(
        std::slice::from_ref(&then),
        &[("", UPDATE)],
        std::slice::from_ref(&then)
    ));
}
