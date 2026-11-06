//! The collect-then-apply pass against a real plan and apply (home
//! in-process in a `Bed`): what is asked, what is sent, and that a `n`,
//! end of input or Ctrl-C sends nothing and writes nothing.

use super::*;
use crate::daemon::home::Home;
use crate::plan::LeafCtx;
use crate::plan::apply::Progress;
use crate::testutil::{self, Bed, InProcess};
use std::cell::RefCell;

#[path = "sync_term_tests.rs"]
mod term;
use term::{Script, answer, ctrl_c};

struct Run {
    code: i32,
    out: String,
    err: String,
    sent: Vec<String>,
}

/// Run the CLI's `sync` against `bed`, answering with `lines`.
fn cli(bed: &Bed, home: &Home, a: &Args, lines: Vec<std::io::Result<String>>) -> Run {
    let sent = RefCell::new(Vec::new());
    let cfg = bed.leaf_cfg();
    let state = bed.leaf_dirs.state();
    let link = InProcess {
        home,
        host: "leaf".into(),
        home_dir: bed.leaf_hd.clone(),
    };
    let ctx = LeafCtx {
        me: "leaf",
        home_dir: &bed.leaf_hd,
        cfg: &cfg,
        state_dir: &state,
        plane: Default::default(),
        accept_rollback: &[],
    };
    let call = |req: LocalRequest, on: &mut dyn FnMut(ProgressEvent)| -> Result<LocalReply> {
        match req {
            LocalRequest::DryRun {
                no_wait,
                accept_rollback,
            } => {
                sent.borrow_mut().push(match accept_rollback.is_empty() {
                    true => "dry-run".into(),
                    false => format!("dry-run accept:{accept_rollback:?}"),
                });
                let ctx = LeafCtx {
                    accept_rollback: &accept_rollback,
                    ..ctx
                };
                Ok(LocalReply::Plan(Box::new(crate::plan::plan(
                    &link, ctx, true, no_wait,
                )?)))
            }
            LocalRequest::Sync { opts } => {
                sent.borrow_mut().push(format!(
                    "sync expect:{} confirm:{:?}{}",
                    opts.expect.is_some(),
                    opts.confirm_delete,
                    match opts.accept_rollback.is_empty() {
                        true => String::new(),
                        false => format!(" accept:{:?}", opts.accept_rollback),
                    }
                ));
                // Events in order, handed to the CLI's sink once the sync
                // returns (a unit's counters come from its transfer's own
                // thread, and the sink is the CLI's, not shareable).
                let events = std::sync::Mutex::new(Vec::new());
                let emit = |e: ProgressEvent| events.lock().unwrap().push(e);
                let progress = Progress::new(&emit, Default::default());
                let r = crate::plan::apply::sync_with(&link, ctx, &opts, &progress);
                for e in events.into_inner().unwrap() {
                    on(e);
                }
                Ok(LocalReply::Synced(Box::new(r?)))
            }
            other => panic!("unexpected {other:?}"),
        }
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut input = Script { lines };
    let code = {
        let mut t = Term {
            input: &mut input,
            out: &mut out,
            err: &mut err,
            style: Style { color: false },
        };
        run(a, &mut t, &call).unwrap()
    };
    Run {
        code,
        out: String::from_utf8(out).unwrap(),
        err: String::from_utf8(err).unwrap(),
        sent: sent.into_inner(),
    }
}

fn tty(bed: &Bed) -> Args {
    Args {
        interactive: true,
        home_dir: bed.leaf_hd.clone(),
        ..Default::default()
    }
}

/// Everything sync could write: both trees, both state dirs.
fn everything(bed: &Bed) -> impl PartialEq + std::fmt::Debug {
    (
        testutil::fingerprint(&bed.home_hd),
        testutil::fingerprint(&bed.leaf_hd),
        testutil::fingerprint(&bed.leaf_dirs.state()),
        testutil::fingerprint(&bed.home_dirs.state().join("markers")),
        testutil::fingerprint(&bed.home_dirs.state().join("leaf-baselines")),
    )
}

fn pull_bed() -> (Bed, Home) {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/r"));
    testutil::write(&bed.home_hd.join("work/notes.txt"), "loose\n");
    let home = bed.home();
    (bed, home)
}

/// A plan with nothing to apply asks nothing: the first sync records the
/// baselines (an `identical` unit is bookkeeping, so that one asks), and
/// the next has no question to put. No answer is read.
#[test]
fn a_plan_with_nothing_to_apply_asks_no_question() {
    let (bed, home) = pull_bed();
    let r = cli(&bed, &home, &tty(&bed), vec![answer("y")]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert_eq!(r.sent, ["dry-run", "sync expect:true confirm:[]"]);
    let before = everything(&bed);
    let r = cli(
        &bed,
        &home,
        &tty(&bed),
        vec![Err(std::io::Error::other("asked"))],
    );
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert_eq!(r.sent, ["dry-run"], "no sync was sent");
    assert!(!r.out.contains("Apply this plan?"), "{}", r.out);
    assert!(r.out.contains("nothing to do with home"), "{}", r.out);
    assert!(r.out.trim_end().ends_with("nothing to apply"), "{}", r.out);
    assert_eq!(everything(&bed), before);
}

#[test]
fn yes_at_the_prompt_applies_the_plan_that_was_shown() {
    let (bed, home) = pull_bed();
    let r = cli(&bed, &home, &tty(&bed), vec![answer("y")]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert_eq!(r.sent, ["dry-run", "sync expect:true confirm:[]"]);
    assert!(r.out.contains("Apply this plan? [y/N]"), "{}", r.out);
    assert!(r.out.contains("done: 2 units applied"), "{}", r.out);
    assert!(bed.leaf_hd.join("work/r/README").exists());
}

#[test]
fn ctrl_c_eof_or_no_at_the_prompt_sends_nothing_and_writes_nothing() {
    for (lines, code) in [
        (vec![ctrl_c()], 1),
        (vec![], 1),
        (vec![answer("n")], 1),
        (vec![answer("")], 1),
        (vec![answer("yep")], 1),
    ] {
        let (bed, home) = pull_bed();
        let before = everything(&bed);
        let r = cli(&bed, &home, &tty(&bed), lines);
        assert_eq!(r.code, code);
        assert_eq!(r.sent, ["dry-run"], "only the plan was asked for");
        assert!(r.out.contains("not applied; nothing written"), "{}", r.out);
        assert_eq!(everything(&bed), before, "not even a marker");
    }
}

/// Pull `work/r`, then delete it here: "deleted on leaf".
fn deletion_bed() -> (Bed, Home) {
    let (bed, home) = pull_bed();
    let y = Args {
        yes: true,
        ..tty(&bed)
    };
    assert_eq!(cli(&bed, &home, &y, vec![]).code, 0);
    std::fs::remove_dir_all(bed.leaf_hd.join("work/r")).unwrap();
    (bed, home)
}

#[test]
fn a_deletion_is_asked_about_by_itself_before_the_plan() {
    let (bed, home) = deletion_bed();
    let r = cli(&bed, &home, &tty(&bed), vec![answer("y"), answer("y")]);
    assert_eq!(r.code, 0, "{}", r.out);
    assert!(
        r.out
            .contains("work/r: deleted on leaf — would remove 1 file on home"),
        "{}",
        r.out
    );
    assert_eq!(r.sent[1], "sync expect:true confirm:[\"work/r\"]");
    assert!(!bed.home_hd.join("work/r/README").exists());
}

#[test]
fn a_deletion_not_confirmed_stops_everything() {
    for lines in [vec![answer("n")], vec![ctrl_c()], vec![]] {
        let (bed, home) = deletion_bed();
        let before = everything(&bed);
        let r = cli(&bed, &home, &tty(&bed), lines);
        assert_eq!(r.code, 2);
        assert_eq!(r.sent, ["dry-run"]);
        assert!(
            r.out.contains("not confirmed; nothing written"),
            "{}",
            r.out
        );
        assert_eq!(everything(&bed), before);
    }
}

#[test]
fn yes_does_not_confirm_a_deletion_but_confirm_delete_does() {
    let (bed, home) = deletion_bed();
    let before = everything(&bed);
    let y = Args {
        yes: true,
        interactive: false,
        ..tty(&bed)
    };
    let r = cli(&bed, &home, &y, vec![]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("--confirm-delete work/r"), "{}", r.err);
    assert!(r.out.is_empty(), "a hook's report goes to stderr");
    assert_eq!(everything(&bed), before);

    let y = Args {
        confirm_delete: vec!["~/work/r".into()],
        ..y
    };
    let r = cli(&bed, &home, &y, vec![]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(r.out.contains("delete on home"), "{}", r.out);
    assert!(!bed.home_hd.join("work/r/README").exists());
}

#[test]
fn without_a_terminal_and_without_yes_it_only_shows_the_plan() {
    let (bed, home) = pull_bed();
    let before = everything(&bed);
    let a = Args {
        interactive: false,
        ..tty(&bed)
    };
    let r = cli(&bed, &home, &a, vec![answer("y")]);
    assert_eq!(r.code, 2);
    assert_eq!(r.sent, ["dry-run"], "never asks, never sends");
    assert!(r.err.contains("rerun with --yes"), "{}", r.err);
    assert!(r.err.contains("pull ← home"), "{}", r.err);
    assert_eq!(everything(&bed), before);
}

#[test]
fn a_blocked_plan_is_not_asked_about() {
    let (bed, home) = pull_bed();
    let l = testutil::repo(&bed.leaf_hd.join("work/r"));
    testutil::git(&l, &["commit", "-q", "--allow-empty", "-m", "leaf only"]);
    let before = everything(&bed);
    let r = cli(&bed, &home, &tty(&bed), vec![answer("y")]);
    assert_eq!(r.code, 2);
    assert!(!r.out.contains("[y/N]"), "{}", r.out);
    assert!(r.out.contains("blocked; nothing written"));
    assert_eq!(everything(&bed), before);
}

#[test]
fn dry_run_only_plans_and_json_needs_an_answerless_mode() {
    let (bed, home) = pull_bed();
    let before = everything(&bed);
    let a = Args {
        dry_run: true,
        ..tty(&bed)
    };
    let r = cli(&bed, &home, &a, vec![]);
    assert_eq!(
        (r.code, r.sent.as_slice()),
        (0, ["dry-run".to_string()].as_slice())
    );
    assert_eq!(everything(&bed), before);

    let j = Args {
        json: true,
        ..tty(&bed)
    };
    let mut t = Term {
        input: &mut Script { lines: vec![] },
        out: &mut Vec::new(),
        err: &mut Vec::new(),
        style: Style { color: false },
    };
    let e = run(&j, &mut t, &|_, _| panic!("nothing is asked of the daemon"))
        .unwrap_err()
        .to_string();
    assert!(e.contains("--json needs --dry-run or --yes"), "{e}");
}

#[path = "sync_rollback_tests.rs"]
mod rollback;

#[path = "sync_resume_tests.rs"]
mod resume;

#[path = "sync_progress_tests.rs"]
mod progress;
