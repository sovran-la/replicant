use super::*;
use crate::plan::{DiffSummary, Timings};
use crate::state::UnitState;
use std::io::Cursor;

fn conflict(rel: &str, kind: UnitKind) -> PlannedUnit {
    PlannedUnit {
        rel: rel.into(),
        kind,
        decision: Decision::Conflict {
            why: "both changed since baseline".into(),
        },
        leaf: UnitState::Present("l".into()),
        home: UnitState::Present("h".into()),
        leaf_head: Some("ref: refs/heads/main".into()),
        home_head: Some("ref: refs/heads/bsneed/coverage".into()),
        leaf_path: format!("/Users/b/{rel}").into(),
        home_path: format!("/home/b/{rel}").into(),
        diff: None::<DiffSummary>,
        refusals: vec![],
        leaf_files: 1,
        home_files: 7,
        resolvable: true,
        leaf_dirty: 0,
        home_dirty: 42,
    }
}

fn plan(units: Vec<PlannedUnit>) -> Plan {
    Plan {
        me: "birchbook".into(),
        home: "thistledown".into(),
        dry_run: true,
        units,
        refused: vec![],
        orphaned: vec![],
        timings: Timings::default(),
    }
}

const PLAIN: Style = Style { color: false };

fn ask_with(p: &Plan, input: &str) -> (Option<Answers>, String) {
    let mut input = Cursor::new(input.as_bytes().to_vec());
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut t = Term {
        input: &mut input,
        out: &mut out,
        err: &mut err,
        style: PLAIN,
    };
    let a = ask_all(&mut t, p).unwrap();
    (a, String::from_utf8(out).unwrap())
}

#[test]
fn answers_by_number_by_name_and_skip_in_any_case() {
    let (me, home) = ("birchbook", "thistledown");
    assert_eq!(parse("1", me, home), Some(Some(Take::Home)));
    assert_eq!(parse("2", me, home), Some(Some(Take::Leaf)));
    assert_eq!(parse("Thistledown", me, home), Some(Some(Take::Home)));
    assert_eq!(parse("BIRCHBOOK", me, home), Some(Some(Take::Leaf)));
    assert_eq!(parse("s", me, home), Some(None));
    assert_eq!(parse("Skip", me, home), Some(None));
    for no in ["", "3", "y", "n", "both", "think"] {
        assert_eq!(parse(no, me, home), None, "{no:?}");
    }
}

#[test]
fn the_question_names_both_machines_their_branches_and_their_dirt() {
    let p = plan(vec![]);
    let q = question(&p, &conflict("work/rust/sysdirs", UnitKind::Repo), PLAIN);
    assert_eq!(
        q,
        "Repo conflict: work/rust/sysdirs (both changed since baseline)\n\
         \x20 thistledown  bsneed/coverage @ 42 dirty\n\
         \x20 birchbook    main @ clean\n\
         \x20 1) use thistledown's copy  (pull: birchbook's is kept in a backup here)\n\
         \x20 2) use birchbook's copy  (push: thistledown's is kept in its push backup)\n\
         \x20 s) skip: leave it as it is, and sync the rest\n"
    );
}

#[test]
fn a_loose_conflict_counts_files_instead() {
    let p = plan(vec![]);
    let q = question(&p, &conflict("work/notes", UnitKind::Loose), PLAIN);
    assert!(q.starts_with("Conflict: work/notes"), "{q}");
    assert!(q.contains("thistledown  7 files\n"), "{q}");
    assert!(q.contains("birchbook    1 file\n"), "{q}");
}

#[test]
fn only_resolvable_conflicts_are_asked_about_in_plan_order() {
    let mut not = conflict("work/a", UnitKind::Repo);
    not.resolvable = false;
    let mut resolved = conflict("work/b", UnitKind::Repo);
    resolved.decision = Decision::Resolved { take: Take::Leaf };
    let p = plan(vec![
        not,
        resolved,
        conflict("work/c", UnitKind::Repo),
        conflict("work/d", UnitKind::Repo),
    ]);
    let rels: Vec<&str> = open(&p).iter().map(|u| u.rel.as_str()).collect();
    assert_eq!(rels, vec!["work/c", "work/d"]);
}

#[test]
fn each_conflict_takes_its_own_answer() {
    let p = plan(vec![
        conflict("work/a", UnitKind::Repo),
        conflict("work/b", UnitKind::Repo),
        conflict("work/c", UnitKind::Repo),
    ]);
    let (a, out) = ask_with(&p, "2\nthistledown\ns\n");
    let a = a.expect("answered");
    assert_eq!(
        a.resolve,
        vec![
            Resolution {
                rel: "work/a".into(),
                take: Take::Leaf
            },
            Resolution {
                rel: "work/b".into(),
                take: Take::Home
            },
        ]
    );
    assert_eq!(a.skip, vec!["work/c"]);
    assert!(a.takes("work/a") && a.takes("work/b") && !a.takes("work/c"));
    assert_eq!(out.matches("Which? [1/2/s] ").count(), 3);
}

#[test]
fn anything_else_is_asked_again() {
    let p = plan(vec![conflict("work/a", UnitKind::Repo)]);
    let (a, out) = ask_with(&p, "y\n\n1\n");
    assert_eq!(a.unwrap().resolve[0].take, Take::Home);
    assert_eq!(out.matches("Which? [1/2/s] ").count(), 3);
    assert_eq!(
        out.matches("answer 1 (thistledown's copy), 2 (birchbook's copy) or s (skip)")
            .count(),
        2
    );
}

/// End of input (or Ctrl-C, an interrupted read) ends the questions, even
/// after earlier answers: nothing is sent.
#[test]
fn end_of_input_ends_the_questions_with_no_answers() {
    let p = plan(vec![
        conflict("work/a", UnitKind::Repo),
        conflict("work/b", UnitKind::Repo),
    ]);
    let (a, _) = ask_with(&p, "2\n");
    assert_eq!(a, None);
    let (a, _) = ask_with(&p, "");
    assert_eq!(a, None);
}

#[test]
fn no_conflicts_asks_nothing() {
    let p = plan(vec![]);
    let (a, out) = ask_with(&p, "");
    assert_eq!(a, Some(Answers::default()));
    assert!(out.is_empty());
}

#[test]
fn what_was_skipped_is_listed_at_the_end() {
    assert_eq!(
        unresolved(&["work/a".into()], PLAIN),
        "1 conflict left unresolved; the next sync asks again:\n  work/a\n"
    );
    let two = unresolved(&["work/a".into(), "work/b".into()], PLAIN);
    assert!(two.starts_with("2 conflicts left unresolved"), "{two}");
}
