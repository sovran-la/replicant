//! A running sync's lines, one event at a time: a started unit's line left
//! open, completed by its end.

use super::*;
use crate::marker::Direction;

fn started(index: usize, total: usize, action: &str, rel: &str) -> ProgressEvent {
    ProgressEvent::UnitStarted {
        rel: rel.into(),
        action: action.into(),
        direction: Some(Direction::Push),
        index,
        total,
    }
}

fn done(
    action: &str,
    files: usize,
    deleted: usize,
    push: Option<&str>,
    o: UnitOutcome,
) -> ProgressEvent {
    ProgressEvent::UnitDone {
        rel: "work/x".into(),
        action: action.into(),
        direction: None,
        files_changed: files,
        deleted,
        elapsed_ms: 1_400,
        push: push.map(Into::into),
        outcome: o,
    }
}

fn plain() -> Style {
    Style { color: false }
}

#[test]
fn a_started_unit_leaves_its_line_open() {
    let s = progress(&started(12, 155, "push", "work/ai_stuff/bonsai"), plain());
    assert_eq!(s, "  [12/155] push  work/ai_stuff/bonsai … ");
    assert!(!s.contains('\n'));
    assert_eq!(
        progress(&started(3, 155, "pull", "work/a"), plain()),
        "  [3/155] pull  work/a … "
    );
    assert_eq!(
        progress(&started(1, 1, "delete on home", "work/a"), plain()),
        "  [1/1] delete on home  work/a … "
    );
}

#[test]
fn a_finished_unit_completes_the_line() {
    let ok = || UnitOutcome::Applied;
    assert_eq!(
        progress(&done("push", 220, 0, None, ok()), plain()),
        "220 files  1.4s\n"
    );
    assert_eq!(
        progress(&done("pull", 1, 0, None, ok()), plain()),
        "1 file  1.4s\n"
    );
    assert_eq!(
        progress(&done("pull", 2, 3, None, ok()), plain()),
        "2 files, 3 deleted  1.4s\n"
    );
    assert_eq!(
        progress(&done("push", 4, 0, Some("p-1"), ok()), plain()),
        "4 files  1.4s  backup p-1\n"
    );
    assert_eq!(
        progress(&done("adopt", 0, 0, None, ok()), plain()),
        "baseline recorded  1.4s\n"
    );
    assert_eq!(
        progress(&done("forget", 0, 0, None, ok()), plain()),
        "baseline dropped  1.4s\n"
    );
    // The whole line, as the terminal shows it.
    let line = progress(&started(12, 155, "push", "work/ai_stuff/bonsai"), plain())
        + &progress(&done("push", 220, 0, None, ok()), plain());
    assert_eq!(
        line,
        "  [12/155] push  work/ai_stuff/bonsai … 220 files  1.4s\n"
    );
}

#[test]
fn a_unit_that_did_not_finish_says_why_in_red() {
    let i = UnitOutcome::Interrupted {
        why: "the CLI hung up; rsync stopped".into(),
    };
    assert_eq!(
        progress(&done("pull", 9, 0, None, i.clone()), plain()),
        "interrupted after 1.4s: the CLI hung up; rsync stopped\n"
    );
    let f = UnitOutcome::Failed {
        why: "home unreachable".into(),
    };
    assert_eq!(
        progress(&done("push", 0, 0, None, f), plain()),
        "failed after 1.4s: home unreachable\n"
    );
    let red = progress(&done("pull", 0, 0, None, i), Style { color: true });
    assert!(
        red.starts_with("\x1b[31m") && red.ends_with('\n'),
        "{red:?}"
    );
    let applied = progress(
        &done("pull", 1, 0, None, UnitOutcome::Applied),
        Style { color: true },
    );
    assert!(
        !applied.contains("\x1b[31m"),
        "only what did not finish is red"
    );
}
