//! Re-review R2: a removal is held like a pull. Unsubscribing a unit whose
//! push home rolled back would delete the leaf's pushed work (home has only
//! its backup of it), and the `Agree` that followed settled the rollback, so
//! the sweep could delete that backup too.

use super::*;
use crate::daemon::wire::{HomeReply, HomeRequest};
use crate::plan::HomeLink;

fn unsubscribe(bed: &Bed, home: &Home, path: &str) {
    let link = testutil::InProcess {
        home,
        host: "leaf".into(),
        home_dir: bed.leaf_hd.clone(),
    };
    let reply = link
        .call(HomeRequest::Unsubscribe {
            paths: vec![path.into()],
        })
        .unwrap();
    assert!(matches!(reply, HomeReply::Unsubscribed(_)), "{reply:?}");
}

fn settled(bed: &Bed, id: &str) -> bool {
    let push: Push = pushes(&bed.home_dirs.state()).get(id).unwrap().unwrap();
    push.rollback_settled_at.is_some()
}

#[test]
fn an_unsubscribe_does_not_remove_a_rolled_back_push_until_accepted() {
    let bed = Bed::enrolled();
    let home = bed.home();
    let id = rolled_back(&bed, &home);
    unsubscribe(&bed, &home, "work");

    let p = plan_of(&bed, &home, true);
    let Decision::Conflict { why } = &unit(&p, "work", UnitKind::Loose).decision else {
        panic!("{:#?}", p.units);
    };
    assert!(
        why.contains(&format!("home rolled back your push {id} at ")),
        "{why}"
    );
    assert!(
        why.contains("`sync --accept-rollback work` lets the removal delete the leaf's copy"),
        "{why}"
    );

    // `--yes` alone: blocked, the leaf untouched, the rollback not settled.
    let before = leaf_side(&bed);
    let r = yes(&bed, &home);
    let Outcome::Blocked { why } = &r.outcome else {
        panic!("{:#?}", r.outcome);
    };
    assert!(
        why.iter()
            .any(|w| w.starts_with("work: ") && w.contains("home rolled back your push")),
        "{why:?}"
    );
    assert!(r.applied.is_empty());
    assert_eq!(leaf_side(&bed), before, "the leaf is untouched");
    assert_eq!(
        std::fs::read_to_string(bed.leaf_hd.join("work/notes.txt")).unwrap(),
        "the leaf's, pushed\n"
    );
    assert!(bed.leaf_hd.join("work/new/deep.txt").exists());
    assert!(!settled(&bed, &id));
    assert!(home.push_dir(&id).join("rollback").is_dir());

    // Accepted: removed, and settled.
    let r = sync_with(&bed, &home, &opts(&["work"])).unwrap();
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    assert!(
        r.applied.iter().any(|a| a.rel == "work"),
        "{:#?}",
        r.applied
    );
    assert!(!bed.leaf_hd.join("work/notes.txt").exists());
    assert!(!bed.leaf_hd.join("work/new/deep.txt").exists());
    assert!(settled(&bed, &id));
    assert!(home.rolled_back_for("leaf").unwrap().is_empty());
}

/// Home's side, by itself: which `Agree` settles a rollback.
#[test]
fn only_an_agree_with_a_baseline_or_an_accepted_one_settles() {
    let bed = Bed::enrolled();
    let home = bed.home();
    let id = rolled_back(&bed, &home);
    let link = testutil::InProcess {
        home: &home,
        host: "leaf".into(),
        home_dir: bed.leaf_hd.clone(),
    };
    let lease = match link.call(HomeRequest::Acquire {
        no_wait: true,
        apply: true,
    }) {
        Ok(HomeReply::Lease { lease, .. }) => lease,
        other => panic!("{other:?}"),
    };
    let agree = |baseline: Option<crate::baseline::Baseline>, accepted: bool| {
        let reply = link.call(HomeRequest::Agree {
            lease,
            rel: "work".into(),
            baseline,
            rollback_accepted: accepted,
        });
        assert!(matches!(reply, Ok(HomeReply::Done)), "{reply:?}");
    };
    // The leaf's unit gone, not accepted: home may hold its last copy.
    agree(None, false);
    assert!(!settled(&bed, &id));
    // Gone, and accepted.
    agree(None, true);
    assert!(settled(&bed, &id));
}
