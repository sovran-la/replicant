//! `--accept-rollback` from the command line (review A2): the dry run the
//! person is shown and the sync sent after their `y` carry the same list,
//! so the confirmed digest matches; `--yes` alone holds the unit.

use super::*;

/// The leaf pushed `work`, and home rolled the push back.
fn rolled_back_bed() -> (Bed, Home) {
    let (bed, home) = pull_bed();
    let y = Args {
        yes: true,
        ..tty(&bed)
    };
    assert_eq!(cli(&bed, &home, &y, vec![]).code, 0);
    testutil::write(&bed.leaf_hd.join("work/notes.txt"), "the leaf's, pushed\n");
    let r = cli(&bed, &home, &y, vec![]);
    assert_eq!(r.code, 0, "{}", r.err);
    let id = home.pushes().unwrap()[0].id.clone();
    home.rollback(&id).unwrap();
    (bed, home)
}

#[test]
fn yes_holds_a_rolled_back_unit_and_accept_rollback_takes_homes_copy() {
    let (bed, home) = rolled_back_bed();
    let before = everything(&bed);
    let y = Args {
        yes: true,
        interactive: false,
        ..tty(&bed)
    };
    let r = cli(&bed, &home, &y, vec![]);
    assert_eq!(r.code, 2, "{}", r.err);
    assert!(r.err.contains("home rolled back your push"), "{}", r.err);
    assert!(r.err.contains("--accept-rollback work"), "{}", r.err);
    assert_eq!(everything(&bed), before, "nothing written");

    let a = Args {
        accept_rollback: vec!["~/work".into()],
        ..y
    };
    let r = cli(&bed, &home, &a, vec![]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert_eq!(r.sent, ["sync expect:false confirm:[] accept:[\"~/work\"]"]);
    assert_eq!(
        std::fs::read_to_string(bed.leaf_hd.join("work/notes.txt")).unwrap(),
        "loose\n"
    );
}

#[test]
fn at_the_prompt_the_plan_shown_and_the_sync_sent_both_accept() {
    let (bed, home) = rolled_back_bed();
    let blocked = cli(&bed, &home, &tty(&bed), vec![answer("y")]);
    assert_eq!(blocked.code, 2);
    assert!(
        blocked.out.contains("blocked; nothing written"),
        "{}",
        blocked.out
    );

    let a = Args {
        accept_rollback: vec!["work".into()],
        ..tty(&bed)
    };
    let r = cli(&bed, &home, &a, vec![answer("y")]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert_eq!(
        r.sent,
        [
            "dry-run accept:[\"work\"]",
            "sync expect:true confirm:[] accept:[\"work\"]"
        ]
    );
    assert!(r.out.contains("done: 1 unit applied"), "{}", r.out);
}
