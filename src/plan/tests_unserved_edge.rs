//! F5 edges: markers over unserved paths, and older peers on the wire.

use super::*;

#[test]
fn an_unsubscribed_path_under_an_interrupted_sync_is_never_remove() {
    let bed = two_subs();
    let home = bed.home();
    baseline_b_as_now(&bed, &home);
    unsubscribe(&bed, &home, "work/b");
    crate::marker::store(&bed.leaf_dirs.state())
        .put(&Marker {
            version: RECORD_VERSION,
            rel: "work/b/r".into(),
            direction: Direction::Pull,
            host: "leaf".into(),
            source_state: UnitState::Present("s".into()),
            started_at: 7,
            push: None,
        })
        .unwrap();
    let p = status(&bed, &home);
    let w = why(unit(&p, "work/b/r"));
    assert!(w.contains("interrupted pull"), "{w}");
    // The loose unit `work/b` contains the marked path: fenced too.
    assert!(why(unit(&p, "work/b")).contains("interrupted pull"));
}

#[test]
fn a_dead_push_marker_on_home_fences_an_unsubscribed_path() {
    let bed = two_subs();
    let home = bed.home();
    baseline_b_as_now(&bed, &home);
    unsubscribe(&bed, &home, "work/b");
    crate::marker::store(&bed.home_dirs.state())
        .put(&Marker {
            version: RECORD_VERSION,
            rel: "work/b/r".into(),
            direction: Direction::Push,
            host: "leaf".into(),
            source_state: UnitState::Present("s".into()),
            started_at: 7,
            push: None,
        })
        .unwrap();
    let w = why(unit(&status(&bed, &home), "work/b/r")).to_string();
    assert!(w.contains("interrupted push"), "{w}");
}

#[test]
fn the_new_wire_fields_default_for_an_older_peer() {
    let req: HomeRequest =
        serde_json::from_str(r#"{"op":"state","leaf_repos":[],"case_check":false}"#).unwrap();
    assert!(matches!(req, HomeRequest::State { baselines, .. } if baselines.is_empty()));
    let snap: HomeSnapshot = serde_json::from_str(
        r#"{"home_dir":"/h","specs":[],"units":[],"repos":[],"worktrees":[],"markers":[]}"#,
    )
    .unwrap();
    assert!(snap.unserved.is_empty());
}

#[test]
fn a_baseline_an_older_home_says_nothing_about_is_not_served() {
    // Home reports no reason (an older home, or a malformed baseline path):
    // the leaf assumes the worst and never removes.
    let bed = two_subs();
    let home = bed.home();
    baseline_b_as_now(&bed, &home);
    unsubscribe(&bed, &home, "work/b");
    struct Old<'a>(InProcess<'a>);
    impl HomeLink for Old<'_> {
        fn call(&self, request: HomeRequest) -> Result<HomeReply> {
            match self.0.call(request)? {
                HomeReply::State(mut s) => {
                    s.unserved.clear();
                    Ok(HomeReply::State(s))
                }
                other => Ok(other),
            }
        }
    }
    let link = Old(InProcess {
        home: &home,
        host: "leaf".into(),
        home_dir: bed.leaf_hd.clone(),
    });
    let cfg = bed.leaf_cfg();
    let state = bed.leaf_dirs.state();
    let ctx = LeafCtx {
        me: "leaf",
        home_dir: &bed.leaf_hd,
        cfg: &cfg,
        state_dir: &state,
        plane: Default::default(),
        accept_rollback: &[],
    };
    let p = plan(&link, ctx, false, false).unwrap();
    assert_eq!(
        unit(&p, "work/b/r").decision,
        Decision::Orphaned {
            reason: UnservedReason::NotServed,
            changed: false
        }
    );
}
