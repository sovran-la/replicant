//! The undo plan by itself, and the backup sweep.

use super::*;

fn run(changes: &[&str], backed: &[&str], absent: &[&str], reported: bool) -> Run {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect();
    Run {
        started_at: 1,
        backed_up: s(backed),
        absent: s(absent),
        dirs: vec![],
        changes: s(changes),
        reported,
    }
}

#[test]
fn undo_follows_the_first_run_that_touched_each_path() {
    let p = Push {
        version: 1,
        id: "leaf/20261101T000000Z".into(),
        host: "leaf".into(),
        unit: "work".into(),
        is_file: false,
        started_at: 1,
        runs: vec![
            run(
                &[
                    "~ a",
                    "+ d/",
                    "+ d/f",
                    "- gone",
                    "~ new-in-new-dir",
                    "~ sneaky",
                    "a d2/",
                ],
                &["a", "gone"],
                &["new-in-new-dir"],
                true,
            ),
            run(&["~ a", "~ d/f", "~ b"], &["a", "d/f", "b"], &[], true),
            run(&[], &["c"], &[], false),
        ],
        finished_at: None,
        rolled_back_at: None,
        rollback_settled_at: None,
    };
    let u = undo(&p);
    assert_eq!(u.restore, vec![vec!["a", "gone"], vec!["b"], vec!["c"]]);
    assert_eq!(
        u.delete,
        vec![
            ("d".to_string(), true),
            ("d/f".to_string(), false),
            ("new-in-new-dir".to_string(), false)
        ]
    );
    assert_eq!(u.unbacked, vec!["sneaky"]);
    assert_eq!(u.unreported, vec![3]);
}

#[test]
fn old_backups_are_swept_unless_their_push_is_still_open() {
    let bed = Bed::enrolled();
    let home = bed.home();
    let now = 2_000_000_000;
    let day = 86_400;
    let mk = |id: &str| {
        testutil::write(&home.push_dir(id).join("run-1/f"), "x");
        pushes(&bed.home_dirs.state())
            .put(&Push {
                version: 1,
                id: id.into(),
                host: "leaf".into(),
                unit: "work".into(),
                is_file: false,
                started_at: 0,
                runs: vec![],
                finished_at: None,
                rolled_back_at: None,
                rollback_settled_at: None,
            })
            .unwrap();
    };
    let old = format!("leaf/{}", crate::timefmt::compact(now - 15 * day));
    let old2 = format!("{old}-2");
    let open = format!("leaf/{}", crate::timefmt::compact(now - 20 * day));
    let young = format!("leaf/{}", crate::timefmt::compact(now - 13 * day));
    for id in [&old, &old2, &open, &young] {
        mk(id);
    }
    testutil::write(&bed.home_dirs.backup().join("leaf/not-a-push/f"), "kept");
    marker::store(&bed.home_dirs.state())
        .put(&Marker {
            version: 1,
            rel: "work".into(),
            direction: crate::marker::Direction::Push,
            host: "leaf".into(),
            source_state: crate::state::UnitState::Empty,
            started_at: 0,
            push: Some(open.clone()),
            backup: None,
        })
        .unwrap();
    assert_eq!(home.sweep_backups(now).unwrap(), 2);
    let store = pushes(&bed.home_dirs.state());
    for (id, kept) in [(&old, false), (&old2, false), (&open, true), (&young, true)] {
        assert_eq!(home.push_dir(id).exists(), kept, "{id}");
        assert_eq!(store.get::<Push>(id).unwrap().is_some(), kept, "{id}");
    }
    assert!(bed.home_dirs.backup().join("leaf/not-a-push/f").exists());
    let log = home.log(&Default::default()).unwrap();
    assert!(
        log.iter()
            .any(|l| l.event == "backup-sweep" && l.detail.contains("removed 2"))
    );

    // backup_days is fleet.toml's.
    testutil::hand_edit(
        &bed.home_dirs.fleet(),
        "listen = ",
        "backup_days = 10\nlisten = ",
    );
    assert_eq!(
        home.sweep_backups(now).unwrap(),
        1,
        "the 13-day-old one now"
    );
    assert_eq!(home.sweep_backups(now).unwrap(), 0);
}

fn push_of(changes: &[&str]) -> Push {
    Push {
        version: 1,
        id: "leaf/20261101T000000Z".into(),
        host: "leaf".into(),
        unit: "work".into(),
        is_file: false,
        started_at: 1,
        runs: vec![run(changes, &[], &[], true)],
        finished_at: None,
        rolled_back_at: None,
        rollback_settled_at: None,
    }
}

#[test]
fn undo_never_deletes_or_restores_outside_the_unit() {
    // Review N2: a manifest written before record_run checked its paths.
    let bad = [
        "+ ../../etc/x",
        "+ /etc/y",
        "- a/../../z",
        "~ ./w",
        "+ d//e",
    ];
    let mut changes = bad.to_vec();
    changes.push("+ ok");
    let u = undo(&push_of(&changes));
    assert_eq!(u.delete, vec![("ok".to_string(), false)]);
    assert!(u.restore.iter().all(Vec::is_empty));
    assert_eq!(u.unbacked.len(), bad.len(), "{:?}", u.unbacked);
}

#[test]
fn record_run_refuses_a_change_outside_the_unit() {
    let bed = Bed::enrolled();
    let home = bed.home();
    let id = "leaf/20261101T000000Z";
    pushes(&bed.home_dirs.state()).put(&push_of(&[])).unwrap();
    marker::store(&bed.home_dirs.state())
        .put(&Marker {
            version: 1,
            rel: "work".into(),
            direction: crate::marker::Direction::Push,
            host: "leaf".into(),
            source_state: crate::state::UnitState::Empty,
            started_at: 0,
            push: Some(id.into()),
            backup: None,
        })
        .unwrap();
    for bad in ["+ ../x", "+ /etc/passwd", "- a/../../b", "~ ./c"] {
        let e = home
            .record_run("leaf", "work", &[bad.to_string(), "+ fine".into()], true)
            .unwrap_err()
            .to_string();
        assert!(e.contains("is not a change inside work"), "{bad}: {e}");
        let p: Push = pushes(&bed.home_dirs.state()).get(id).unwrap().unwrap();
        assert_eq!(p, push_of(&[]), "{bad}: nothing recorded");
    }
    let ok = [
        "+ d/".to_string(),
        "+ d/f".into(),
        "~ a".into(),
        "- b".into(),
    ];
    assert_eq!(
        home.record_run("leaf", "work", &ok, true)
            .unwrap()
            .as_deref(),
        Some(id)
    );
    let p: Push = pushes(&bed.home_dirs.state()).get(id).unwrap().unwrap();
    assert_eq!(p.runs[0].changes, ok);
    assert!(p.finished_at.is_some());
}

fn run_of(changes: &[&str], backed: &[&str]) -> Run {
    run(changes, backed, &[], true)
}

#[test]
fn undo_of_a_kind_change_deletes_the_new_kind_and_restores_the_old() {
    let mut p = push_of(&[]);
    p.runs = vec![run_of(
        &["- a/", "+ a", "- a/x", "- f", "+ f/", "+ f/y"],
        &["a/x", "f"],
    )];
    let u = undo(&p);
    assert_eq!(
        u.delete,
        [
            ("a".to_string(), false),
            ("f".to_string(), true),
            ("f/y".to_string(), false),
        ]
    );
    assert_eq!(u.restore, [vec!["a/x".to_string(), "f".to_string()]]);
    assert!(u.unbacked.is_empty());
}

#[test]
fn undo_across_runs_goes_by_the_first_but_clears_what_a_later_run_made() {
    let mut p = push_of(&[]);
    // Run 1 replaced `f`; run 2 made it a directory. Run 1 made `n`; run 2
    // replaced it, keeping run 1's copy, which is no one's original.
    p.runs = vec![
        run_of(&["~ f", "+ n"], &["f"]),
        run_of(&["- f", "+ f/", "~ n"], &["f", "n"]),
    ];
    let u = undo(&p);
    assert_eq!(
        u.delete,
        [("f".to_string(), true), ("n".to_string(), false)]
    );
    assert_eq!(u.restore, [vec!["f".to_string()], vec![]]);
}

#[test]
fn undo_takes_directories_from_what_the_run_noted_first() {
    use crate::native::backup::DirWas;
    let d = |rel: &str, mode| DirWas {
        rel: rel.into(),
        mode,
    };
    let mut p = push_of(&[]);
    p.runs = vec![
        run_of(&["- gone/", "a modes/", "+ made/"], &[]),
        run_of(&[], &[]),
    ];
    p.runs[0].dirs = vec![
        d("gone", Some(0o700)),
        d("modes", Some(0o750)),
        d("made", None),
    ];
    p.runs[1].dirs = vec![
        d("gone", None),
        d("made", Some(0o777)),
        d("late", None),
        d("../out", None),
    ];
    let u = undo(&p);
    assert_eq!(
        u.dirs,
        [("gone".to_string(), 0o700), ("modes".to_string(), 0o750)]
    );
    assert_eq!(
        u.delete,
        [("late".to_string(), true), ("made".to_string(), true)],
        "and nothing outside the unit"
    );
}
