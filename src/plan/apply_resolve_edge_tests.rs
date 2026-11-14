//! Resolutions that may not go: an answer that no longer fits the plan, a
//! conflict nobody answered, one with an empty side; and what they read
//! from an older CLI.

use super::super::block::untouched;
use super::*;

/// An answer made against one plan, sent with another: nothing written.
#[test]
fn an_answer_that_names_no_open_conflict_writes_nothing() {
    let (bed, home, ..) = conflicted();
    testutil::write(&bed.home_hd.join("work/notes.txt"), "would pull\n");
    let cases = [
        (
            take("work/nope", Take::Leaf),
            "work/nope: answered as a conflict, but no unit has that path",
        ),
        (
            take("work", Take::Home),
            "work: answered as a conflict, but it is pull now; run `replicant sync` again",
        ),
        (
            SyncOpts {
                skip: vec!["work".into()],
                ..Default::default()
            },
            "work: skipped as a conflict, but it is pull now; run `replicant sync` again",
        ),
        (
            SyncOpts {
                skip: vec!["work/nope".into()],
                ..Default::default()
            },
            "work/nope: skipped as a conflict, but no unit has that path",
        ),
    ];
    for (opts, want) in cases {
        let r = untouched(&bed, || sync_with(&bed, &home, &opts).unwrap());
        let Outcome::Blocked { why } = &r.outcome else {
            panic!("not blocked: {:#?}", r.outcome);
        };
        assert!(why.iter().any(|w| w == want), "{why:#?}");
    }
}

/// Without an answer a conflict still stops everything: `--yes`, the
/// agents' hooks, never resolve one.
#[test]
fn an_unanswered_conflict_still_blocks_the_whole_sync() {
    let (bed, home, ..) = conflicted();
    testutil::write(&bed.home_hd.join("work/notes.txt"), "would pull\n");
    let r = untouched(&bed, || yes(&bed, &home));
    assert_eq!(r.outcome.exit_code(), 2);
}

/// A rolled-back push has its own answer (`--accept-rollback`), and an
/// empty side cannot be taken: neither is answered here.
#[test]
fn a_resolution_cannot_take_an_empty_side() {
    let (bed, home, h, _) = conflicted();
    std::fs::remove_dir_all(&h).unwrap();
    let p = plan_of(&bed, &home, true);
    let u = unit(&p, "work/r", UnitKind::Repo);
    assert!(!u.resolvable, "{:?}", u.decision);
    let r = untouched(&bed, || {
        sync_with(&bed, &home, &take("work/r", Take::Home)).unwrap()
    });
    assert!(matches!(r.outcome, Outcome::Blocked { .. }));
}

/// The leaf's backups sit where `Dirs` puts them, beside its state.
#[test]
fn the_leafs_backup_dir_is_dirs_backup() {
    let bed = Bed::enrolled();
    let cfg = bed.leaf_cfg();
    let state = bed.leaf_dirs.state();
    assert_eq!(ctx(&bed, &cfg, &state).backup_dir(), bed.leaf_dirs.backup());
}

/// A CLI from before rev 22 sends no answers: they read as none.
#[test]
fn options_without_answers_read_as_none() {
    let o: SyncOpts =
        serde_json::from_str(r#"{"no_wait":false,"confirm_delete":[],"expect":null}"#).unwrap();
    assert!(o.resolve.is_empty() && o.skip.is_empty());
    let m: Marker = serde_json::from_str(
        r#"{"version":1,"rel":"work/r","direction":"pull","host":"leaf",
            "source_state":"empty","started_at":1}"#,
    )
    .unwrap();
    assert_eq!(m.backup, None);
}
