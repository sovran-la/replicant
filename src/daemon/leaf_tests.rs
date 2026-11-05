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

#[test]
fn replaced_addresses_names_the_saved_list_only_when_it_loses_one() {
    let a = |l: &[&str]| Addresses::new(l.iter().map(|s| s.to_string()).collect()).unwrap();
    // Nothing saved: a first subscribe replaces nothing (review A28).
    assert!(replaced_addresses(None, &a(&["x:1"])).is_empty());
    // Every saved address still listed, in any order or with more: nothing.
    assert!(replaced_addresses(Some(&a(&["x:1", "y:1"])), &a(&["y:1", "x:1", "z:1"])).is_empty());
    // One dropped: the whole saved list, so the terminal can show it.
    assert_eq!(
        replaced_addresses(Some(&a(&["x:1", "y:1"])), &a(&["x:1"])),
        vec!["x:1".to_string(), "y:1".to_string()]
    );
    // Same length, different address: still a replacement (review N12c).
    assert_eq!(
        replaced_addresses(Some(&a(&["x:1"])), &a(&["y:1"])),
        vec!["x:1".to_string()]
    );
}
