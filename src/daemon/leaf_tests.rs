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

/// The call site of `replaced_addresses`: a first subscribe, whose list was
/// made up from the home's name a moment ago, replaces nothing even when
/// `--address` names something else; a saved list that loses an address is
/// named; no `--address` keeps the list (review A47, pinning A28).
#[test]
fn replace_address_reports_a_saved_list_only() {
    let fresh = |address: &[&str]| LeafConfig {
        name: None,
        home_dir: None,
        home: HomeEntry {
            name: "home".into(),
            address: Addresses::new(address.iter().map(|s| s.to_string()).collect()).unwrap(),
            credentials: None,
            rsync_host: None,
            rsync_timeout_secs: None,
            streams: None,
        },
        as_local: Default::default(),
    };
    // No leaf.toml yet: the default `home:7750` was never saved.
    let mut cfg = fresh(&["home:7750"]);
    let r = replace_address(&mut cfg, false, vec!["lan:7750".into()]).unwrap();
    assert!(r.is_empty(), "{r:?}");
    assert_eq!(cfg.home.address.as_slice(), ["lan:7750"]);
    // Saved, and losing one: named.
    let mut cfg = fresh(&["lan:7750", "tail:7750"]);
    let r = replace_address(&mut cfg, true, vec!["lan:7750".into()]).unwrap();
    assert_eq!(r, vec!["lan:7750".to_string(), "tail:7750".to_string()]);
    assert_eq!(cfg.home.address.as_slice(), ["lan:7750"]);
    // No --address: kept, nothing replaced.
    let r = replace_address(&mut cfg, true, vec![]).unwrap();
    assert!(r.is_empty());
    assert_eq!(cfg.home.address.as_slice(), ["lan:7750"]);
    // A bad address is refused before anything changes.
    let e = replace_address(&mut cfg, true, vec!["   ".into()]).unwrap_err();
    assert!(e.to_string().starts_with("config: --address:"), "{e}");
    assert_eq!(cfg.home.address.as_slice(), ["lan:7750"]);
}
