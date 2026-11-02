//! The collect-then-apply pass against a real plan and apply (home
//! in-process in a `Bed`): what is asked, what is sent, and that a `n`,
//! end of input or Ctrl-C sends nothing and writes nothing.

use super::*;
use crate::daemon::home::Home;
use crate::plan::{LeafCtx, RSYNC};
use crate::testutil::{self, Bed, InProcess};
use std::cell::RefCell;

/// A terminal: scripted input (a read error stands for Ctrl-C), captured
/// output.
struct Script {
    lines: Vec<std::io::Result<String>>,
}

impl std::io::Read for Script {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        unreachable!("read_line only")
    }
}

impl BufRead for Script {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        unreachable!("read_line only")
    }
    fn consume(&mut self, _: usize) {}
    fn read_line(&mut self, buf: &mut String) -> std::io::Result<usize> {
        if self.lines.is_empty() {
            return Ok(0);
        }
        let l = self.lines.remove(0)?;
        buf.push_str(&l);
        Ok(l.len())
    }
}

fn ctrl_c() -> std::io::Result<String> {
    Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
}

fn answer(s: &str) -> std::io::Result<String> {
    Ok(format!("{s}\n"))
}

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
    let (state, scratch) = (bed.leaf_dirs.state(), bed.leaf_dirs.scratch());
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
        scratch_dir: &scratch,
        rsync: RSYNC,
    };
    let call = |req: LocalRequest| -> Result<LocalReply> {
        match req {
            LocalRequest::DryRun { no_wait } => {
                sent.borrow_mut().push("dry-run".into());
                Ok(LocalReply::Plan(Box::new(crate::plan::plan(
                    &link, ctx, true, no_wait,
                )?)))
            }
            LocalRequest::Sync { opts } => {
                sent.borrow_mut().push(format!(
                    "sync expect:{} confirm:{:?}",
                    opts.expect.is_some(),
                    opts.confirm_delete
                ));
                Ok(LocalReply::Synced(Box::new(crate::plan::apply::sync(
                    &link, ctx, &opts,
                )?)))
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
    let e = run(&j, &mut t, &|_| panic!("nothing is asked of the daemon"))
        .unwrap_err()
        .to_string();
    assert!(e.contains("--json needs --dry-run or --yes"), "{e}");
}
