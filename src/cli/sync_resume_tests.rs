//! Guardrail 6 at the prompt (review A3): a resume that would replace what
//! changed here since it was interrupted is blocked, not asked about, and
//! `--confirm-delete <unit>` waives it for the plan shown and the sync sent.

use super::*;
use crate::plan::apply::tests::marker_tests::interrupted;

#[test]
fn a_held_resume_is_not_asked_about_and_confirm_delete_lets_it_through() {
    let (bed, home) = interrupted("work", "/work/", "work/notes.txt");
    let later = bed.leaf_hd.join("work/todo-made-later.md");
    testutil::write(&later, "later\n");
    let before = everything(&bed);
    let r = cli(&bed, &home, &tty(&bed), vec![answer("y")]);
    assert_eq!(r.code, 2);
    assert_eq!(r.sent, ["dry-run"]);
    assert!(!r.out.contains("[y/N]"), "{}", r.out);
    assert!(r.out.contains("blocked; nothing written"), "{}", r.out);
    assert!(r.out.contains("todo-made-later.md"), "{}", r.out);
    assert_eq!(everything(&bed), before);

    let a = Args {
        confirm_delete: vec!["work".into()],
        ..tty(&bed)
    };
    let r = cli(&bed, &home, &a, vec![answer("y")]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert_eq!(r.sent, ["dry-run", "sync expect:true confirm:[\"work\"]"]);
    assert!(!later.exists());
}
