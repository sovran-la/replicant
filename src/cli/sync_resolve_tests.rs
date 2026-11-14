//! The conflict question at a terminal, against a real plan and apply
//! (rev 22): each answer is planned again and shown, then confirmed with
//! the plan; a skip lets the rest go and the run ends saying what was left;
//! Ctrl-C or a `n` anywhere sends nothing.

use super::*;

/// A repo synced once, then changed on both sides. Returns (bed, home,
/// home's repo, the leaf's).
fn conflicted() -> (Bed, Home, std::path::PathBuf, std::path::PathBuf) {
    let bed = Bed::enrolled();
    let h = testutil::repo(&bed.home_hd.join("work/r"));
    // The loose unit holds something, as a real ~/work does: an empty one
    // reads as "both empty", which is always bookkeeping to apply.
    testutil::write(&bed.home_hd.join("work/notes.txt"), "loose\n");
    let home = bed.home();
    let first = cli(&bed, &home, &tty(&bed), vec![answer("y")]);
    assert_eq!(first.code, 0, "{}{}", first.out, first.err);
    let l = bed.leaf_hd.join("work/r");
    testutil::write(&l.join("README"), "the leaf's newer README\n");
    testutil::git(&l, &["commit", "-qam", "leaf moved"]);
    testutil::write(&h.join("README"), "home's uncommitted README\n");
    (bed, home, h, l)
}

fn read(p: &std::path::Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

#[test]
fn answering_2_takes_this_copy_shows_it_and_pushes_it() {
    let (bed, home, h, _) = conflicted();
    let r = cli(&bed, &home, &tty(&bed), vec![answer("2"), answer("y")]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert_eq!(
        r.sent,
        [
            "dry-run",
            "dry-run resolve:work/r=leaf",
            "sync expect:true confirm:[] resolve:work/r=leaf skip:[]",
        ]
    );
    let asked = r.out.find("Repo conflict: work/r").expect("asked");
    let shown = r
        .out
        .find("resolved:\n  push (resolved: this copy)")
        .expect("shown");
    let confirmed = r.out.find("Apply this plan?").expect("confirmed");
    assert!(asked < shown && shown < confirmed, "{}", r.out);
    assert_eq!(read(&h.join("README")), "the leaf's newer README\n");
}

#[test]
fn answering_1_takes_homes_copy_and_pulls_it() {
    let (bed, home, _, l) = conflicted();
    let r = cli(&bed, &home, &tty(&bed), vec![answer("1"), answer("y")]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert!(r.out.contains("pull (resolved: home's copy)"), "{}", r.out);
    assert_eq!(read(&l.join("README")), "home's uncommitted README\n");
    let kept = std::fs::read_dir(bed.leaf_dirs.backup().join("resolved"))
        .unwrap()
        .count();
    assert_eq!(kept, 1, "the leaf's copy is in a backup here");
}

#[test]
fn a_skip_lets_the_rest_go_and_the_run_ends_listing_it_with_exit_2() {
    let (bed, home, h, l) = conflicted();
    testutil::write(&bed.home_hd.join("work/notes.txt"), "home changed it\n");
    let before = (testutil::fingerprint(&h), testutil::fingerprint(&l));
    let r = cli(&bed, &home, &tty(&bed), vec![answer("s"), answer("y")]);
    assert_eq!(r.code, 2, "{}{}", r.out, r.err);
    assert_eq!(
        r.sent,
        [
            "dry-run",
            "sync expect:true confirm:[] resolve: skip:[\"work/r\"]"
        ],
        "{}",
        r.out
    );
    assert_eq!(
        read(&bed.leaf_hd.join("work/notes.txt")),
        "home changed it\n"
    );
    assert_eq!(
        before,
        (testutil::fingerprint(&h), testutil::fingerprint(&l))
    );
    assert!(
        r.out
            .trim_end()
            .ends_with("1 conflict left unresolved; the next sync asks again:\n  work/r"),
        "{}",
        r.out
    );
}

#[test]
fn skipping_the_only_thing_to_do_sends_nothing_and_still_says_so() {
    let (bed, home, ..) = conflicted();
    let before = everything(&bed);
    let r = cli(&bed, &home, &tty(&bed), vec![answer("skip")]);
    assert_eq!(r.code, 2, "{}{}", r.out, r.err);
    assert_eq!(r.sent, ["dry-run"]);
    assert!(
        r.out
            .contains("nothing to apply\n1 conflict left unresolved"),
        "{}",
        r.out
    );
    assert_eq!(everything(&bed), before);
}

#[test]
fn ctrl_c_at_the_question_or_no_at_the_plan_writes_nothing() {
    let (bed, home, ..) = conflicted();
    let before = everything(&bed);
    let r = cli(&bed, &home, &tty(&bed), vec![ctrl_c()]);
    assert_eq!(r.code, 1, "{}{}", r.out, r.err);
    assert_eq!(r.sent, ["dry-run"]);
    assert!(r.out.contains("not answered; nothing written"), "{}", r.out);
    let r = cli(&bed, &home, &tty(&bed), vec![answer("2"), answer("n")]);
    assert_eq!(r.code, 1, "{}{}", r.out, r.err);
    assert_eq!(r.sent, ["dry-run", "dry-run resolve:work/r=leaf"]);
    assert_eq!(everything(&bed), before);
}

/// Nobody to ask: the conflict blocks, exactly as before (rev 22 changes
/// nothing for `--non-interactive`, `--yes`, or the agents' hooks).
#[test]
fn without_a_terminal_a_conflict_is_not_asked_about_and_blocks() {
    let (bed, home, ..) = conflicted();
    let before = everything(&bed);
    let a = Args {
        interactive: false,
        ..tty(&bed)
    };
    let r = cli(&bed, &home, &a, vec![]);
    assert_eq!(r.code, 2, "{}{}", r.out, r.err);
    assert_eq!(r.sent, ["dry-run"]);
    assert!(r.err.contains("blocked; nothing written"), "{}", r.err);
    assert!(!r.err.contains("Which?"), "{}", r.err);
    assert_eq!(everything(&bed), before);
}

/// A conflict with an empty side is no question to put: taking that side
/// would delete the unit.
#[test]
fn a_conflict_that_cannot_take_an_answer_is_not_asked_about() {
    let (bed, home, h, _) = conflicted();
    std::fs::remove_dir_all(&h).unwrap();
    let r = cli(&bed, &home, &tty(&bed), vec![]);
    assert_eq!(r.code, 2, "{}{}", r.out, r.err);
    assert!(!r.out.contains("Which?"), "{}", r.out);
    assert!(r.out.contains("blocked; nothing written"), "{}", r.out);
}
