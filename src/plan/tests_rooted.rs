//! Review P3: a root exclude with a `/` is relative to the root, whatever
//! subscription it is applied under. `agents/state` on the `.sovran` root is
//! never synced, for a host subscribed to `.sovran` or to `.sovran/agents`.

use super::*;

/// The bed plus a `sovran` root excluding `agents/state`, with `leaf`
/// subscribed to `subs` instead of `work`. Home holds daemon state, a repo
/// inside it, and real content beside it.
fn sovran(subs: &[&str]) -> Bed {
    let bed = Bed::enrolled();
    let s = bed.home_hd.join(".sovran");
    testutil::write(&s.join("agents/state/db"), "per-machine\n");
    testutil::repo(&s.join("agents/state/r"));
    testutil::write(&s.join("agents/types/t.toml"), "shared\n");
    testutil::write(&s.join("top.toml"), "shared\n");
    let list = subs
        .iter()
        .map(|p| format!("{p:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    let fleet = bed.home_dirs.fleet();
    let text = std::fs::read_to_string(&fleet)
        .unwrap()
        .replace(
            "[hosts.leaf]",
            &format!(
                "[roots.sovran]\npath = \"{}\"\nexclude = [\"agents/state\"]\n\n[hosts.leaf]",
                s.display()
            ),
        )
        .replace("paths = [\"work\"]", &format!("paths = [{list}]"));
    std::fs::write(&fleet, text).unwrap();
    bed
}

fn changes(p: &Plan, rel: &str) -> Vec<String> {
    p.units
        .iter()
        .find(|u| u.rel == rel && u.kind == UnitKind::Loose)
        .and_then(|u| u.diff.as_ref())
        .unwrap_or_else(|| panic!("no loose diff for {rel}: {:#?}", p.units))
        .changes
        .clone()
}

fn assert_no_state(p: &Plan, rel: &str) {
    assert!(
        !p.units.iter().any(|u| u.rel.contains("agents/state")),
        "a unit inside the exclude: {:#?}",
        p.units
    );
    let c = changes(p, rel);
    assert!(
        !c.iter().any(|x| x.contains("state")),
        "{rel}: daemon state would sync: {c:?}"
    );
    assert!(c.iter().any(|x| x.contains("t.toml")), "{rel}: {c:?}");
}

#[test]
fn a_slash_exclude_holds_for_a_host_subscribed_below_the_root() {
    let bed = sovran(&[".sovran/agents"]);
    assert_no_state(&dry(&bed), ".sovran/agents");
}

#[test]
fn a_slash_exclude_holds_for_a_host_subscribed_to_the_root() {
    let bed = sovran(&[".sovran"]);
    let p = dry(&bed);
    assert_no_state(&p, ".sovran");
    assert!(
        changes(&p, ".sovran")
            .iter()
            .any(|x| x.ends_with("top.toml"))
    );
}

#[test]
fn a_slash_exclude_does_not_reach_a_sibling_subscription() {
    // `agents/state` is `.sovran/agents/state`, not `<anything>/agents/state`.
    let bed = sovran(&[".sovran/x"]);
    testutil::write(&bed.home_hd.join(".sovran/x/agents/state/keep"), "k\n");
    let c = changes(&dry(&bed), ".sovran/x");
    assert!(c.iter().any(|x| x.ends_with("agents/state/keep")), "{c:?}");
}

#[test]
fn subscribing_inside_a_root_exclude_is_refused_up_front() {
    let bed = sovran(&[".sovran/agents"]);
    let home = bed.home();
    let link = InProcess {
        home: &home,
        host: "leaf".into(),
        home_dir: bed.leaf_hd.clone(),
    };
    let before = std::fs::read_to_string(bed.home_dirs.fleet()).unwrap();
    for p in [".sovran/agents/state", ".sovran/agents/state/r"] {
        let err = link
            .call(HomeRequest::Subscribe {
                paths: vec![bed.leaf_hd.join(p).display().to_string()],
            })
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(&format!(
                "{p} is inside [roots.sovran] exclude \"agents/state\": never synced"
            )),
            "{err}"
        );
    }
    let after = std::fs::read_to_string(bed.home_dirs.fleet()).unwrap();
    assert_eq!(before, after, "nothing written");
}

#[test]
fn a_subscription_an_exclude_now_covers_is_refused_when_served() {
    // Written before the exclude existed (here: by hand), it is not served:
    // it shows as orphaned, and so does its baseline: never a delete.
    let bed = sovran(&[".sovran/agents/state", ".sovran/agents/types"]);
    put_baseline(
        &bed,
        ".sovran/agents/state/r",
        UnitState::Present("l".into()),
        UnitState::Present("h".into()),
    );
    let p = go(&bed, &bed.home(), &bed.leaf_cfg(), false).unwrap();
    assert!(p.refused.is_empty(), "{:?}", p.refused);
    assert_eq!(p.orphaned.len(), 1, "{:?}", p.orphaned);
    assert_eq!(p.orphaned[0].unit, ".sovran/agents/state");
    assert!(
        p.orphaned[0].detail[0].contains("inside [roots.sovran] exclude \"agents/state\""),
        "{:?}",
        p.orphaned[0]
    );
    assert!(
        !p.blocked(),
        "home's policy, not a broken config: an orphan"
    );
    assert_eq!(
        unit(&p, ".sovran/agents/state/r").decision,
        Decision::Orphaned {
            reason: crate::decide::UnservedReason::Excluded,
            changed: true
        }
    );
    assert!(p.units.iter().any(|u| u.rel == ".sovran/agents/types"));
}
