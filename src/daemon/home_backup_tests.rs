//! The undo plan by itself, and the backup sweep.

use super::*;

fn run(changes: &[&str], backed: &[&str], absent: &[&str], reported: bool) -> Run {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect();
    Run {
        started_at: 1,
        backed_up: s(backed),
        absent: s(absent),
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
