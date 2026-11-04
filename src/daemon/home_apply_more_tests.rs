//! Notes, interruptions and rehashing through home's handler.

use super::*;

#[test]
fn notes_are_limited_and_interruptions_need_no_lease() {
    let bed = two();
    let home = bed.home();
    let note = |event: &str| HomeRequest::Note {
        event: event.into(),
        rel: "work/r".into(),
        detail: "d".into(),
    };
    for forged in ["approve", "sync", "expired", "rollback"] {
        let e = as_host(&home, &bed, "leaf", note(forged)).unwrap_err();
        assert!(e.contains("may not journal"), "{forged}: {e}");
    }
    as_host(&home, &bed, "leaf", note("adopt")).unwrap();
    // No lease at all: the evidence still lands.
    let a = call(
        &home,
        "leaf",
        &bed.leaf_hd,
        HomeRequest::Interrupted {
            rel: "work/r".into(),
            direction: Direction::Push,
            why: "the other side closed the data connection".into(),
            changes: vec![],
        },
    );
    assert!(a.result.is_ok());
    assert!(
        a.journal[0]
            .contains("push interrupted (the other side closed the data connection); marker kept")
    );
    let log = home.log(&Filter::default()).unwrap();
    let events: Vec<&str> = log.iter().map(|l| l.event.as_str()).collect();
    assert!(events.ends_with(&["adopt", "sync"]), "{events:?}");
}

#[test]
fn rehash_answers_the_state_home_s_snapshot_has() {
    let bed = two();
    testutil::repo(&bed.home_hd.join("work/r"));
    testutil::write(&bed.home_hd.join("work/notes.txt"), "loose\n");
    let home = bed.home();
    let l = lease(&home, &bed, "leaf");
    let HomeReply::State(snap) = ok(call(
        &home,
        "leaf",
        &bed.leaf_hd,
        HomeRequest::State {
            leaf_repos: vec![],
            case_check: false,
            baselines: vec![],
        },
    )) else {
        panic!()
    };
    for (rel, kind) in [("work/r", UnitKind::Repo), ("work", UnitKind::Loose)] {
        let want = &snap
            .units
            .iter()
            .find(|u| u.rel == rel && u.kind == kind)
            .unwrap()
            .state;
        let got = as_host(
            &home,
            &bed,
            "leaf",
            HomeRequest::Rehash {
                lease: l,
                rel: rel.into(),
                kind,
                union: vec!["work/r".into(), "../escape".into()],
            },
        )
        .unwrap();
        let HomeReply::Rehashed { state } = got else {
            panic!("{got:?}")
        };
        assert_eq!(&state, want, "{rel}");
    }
}
