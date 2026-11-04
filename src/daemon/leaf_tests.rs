//! The leaf's own journal copy of an interruption home did not hear of.

use super::*;

#[test]
fn an_interruption_home_did_not_hear_of_is_kept_here() {
    let tmp = tempfile::tempdir().unwrap();
    let j = Journal::at(tmp.path().join("journal.log"));
    let heard = Outcome::Interrupted {
        rel: "work/a".into(),
        why: "the other side closed the data connection".into(),
        journaled: true,
    };
    journal_unheard(&j, "leaf", &heard);
    journal_unheard(&j, "leaf", &Outcome::Done);
    assert!(
        j.read(&Default::default()).unwrap().is_empty(),
        "home's copy comes back"
    );
    let unheard = Outcome::Interrupted {
        rel: "work/a".into(),
        why: "the other side closed the data connection".into(),
        journaled: false,
    };
    journal_unheard(&j, "leaf", &unheard);
    let lines = j.read(&Default::default()).unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(
        (
            lines[0].event.as_str(),
            lines[0].host.as_str(),
            lines[0].path.as_str()
        ),
        ("sync", "leaf", "work/a")
    );
    assert!(
        lines[0]
            .detail
            .contains("interrupted (the other side closed the data connection)")
    );
}
