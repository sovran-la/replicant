//! F11: a hand edit to fleet.toml is honored by a running `Home` on its next
//! request, with no restart and no write of its own in between.

use super::*;
use crate::testutil::{self, Bed};

fn call(home: &Home, host: &str, hd: &std::path::Path, request: HomeRequest) -> HomeAnswer {
    home.handle(HomeCall {
        version: WIRE_VERSION,
        host: host.into(),
        home_dir: hd.to_path_buf(),
        request,
    })
}

fn served(home: &Home, bed: &Bed) -> Vec<String> {
    match call(home, "leaf", &bed.leaf_hd, HomeRequest::Subscriptions).result {
        Ok(HomeReply::Subscriptions(v)) => v.specs.into_iter().map(|s| s.rel).collect(),
        other => panic!("{other:?}"),
    }
}

/// Rewrite fleet.toml in place (same inode, as `>` or an editor that does not
/// rename would) and move its mtime a full second, so the edit is visible
/// whatever the filesystem's timestamp granularity.
fn hand_edit(bed: &Bed, from: &str, to: &str) {
    let path = bed.home_dirs.fleet();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains(from), "{from:?} not in fleet.toml");
    let was = std::fs::metadata(&path).unwrap().modified().unwrap();
    let f = std::fs::File::options()
        .write(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    std::io::Write::write_all(&mut &f, text.replace(from, to).as_bytes()).unwrap();
    f.set_modified(was + Duration::from_secs(1)).unwrap();
}

fn two_subscriptions() -> Bed {
    let bed = Bed::enrolled();
    testutil::repo(&bed.home_hd.join("work/a/r"));
    testutil::repo(&bed.home_hd.join("work/b/r"));
    let path = bed.home_dirs.fleet();
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("paths = [\"work\"]", "paths = [\"work/a\", \"work/b\"]");
    std::fs::write(&path, text).unwrap();
    bed
}

#[test]
fn a_cap_narrowed_by_hand_is_enforced_on_the_next_request() {
    let bed = two_subscriptions();
    let home = bed.home();
    assert_eq!(served(&home, &bed), ["work/a", "work/b"]);

    hand_edit(
        &bed,
        "[hosts.leaf]\n",
        "[hosts.leaf]\nallow = [\"work/a\"]\n",
    );
    assert_eq!(served(&home, &bed), ["work/a"], "Subscriptions");

    let st = call(
        &home,
        "leaf",
        &bed.leaf_hd,
        HomeRequest::State {
            leaf_repos: vec!["work/b/r".into()],
            case_check: false,
        },
    );
    let Ok(HomeReply::State(s)) = st.result else {
        panic!("{:?}", st.result)
    };
    assert_eq!(s.repos, vec!["work/a/r"], "State");
    assert!(s.units.iter().all(|u| u.rel.starts_with("work/a")));

    let acq = call(
        &home,
        "leaf",
        &bed.leaf_hd,
        HomeRequest::Acquire { no_wait: true },
    );
    assert!(acq.result.is_ok(), "{:?}", acq.result);
    assert_eq!(home.queue.view()[0].paths, vec!["work/a"], "Acquire");
}

#[test]
fn a_same_size_edit_is_seen_by_its_mtime() {
    let bed = two_subscriptions();
    let home = bed.home();
    assert_eq!(served(&home, &bed), ["work/a", "work/b"]);
    hand_edit(&bed, "\"work/a\", \"work/b\"", "\"work/b\", \"work/a\"");
    hand_edit(
        &bed,
        "[hosts.leaf]\n",
        "[hosts.leaf]\nallow = [\"work/b\"]\n",
    );
    assert_eq!(served(&home, &bed), ["work/b"]);
    // Now the same length: "work/b" → "work/a".
    hand_edit(&bed, "allow = [\"work/b\"]", "allow = [\"work/a\"]");
    assert_eq!(served(&home, &bed), ["work/a"]);
}

#[test]
fn approve_previews_and_writes_from_the_file_not_the_cache() {
    let bed = Bed::new("");
    let home = bed.home();
    assert!(home.pending().unwrap().is_empty());
    // A pending entry and a new root, both added by hand after `open`.
    std::fs::create_dir_all(bed.home_hd.join("src")).unwrap();
    hand_edit(
        &bed,
        "[roots.work]\n",
        &format!(
            "[roots.src]\npath = \"{}/src\"\n\n[pending.newbox]\nhome_dir = \"{}\"\n\
             paths = [\"src\"]\nrequested_at = 1\n\n[roots.work]\n",
            bed.home_hd.display(),
            bed.leaf_hd.display()
        ),
    );
    let pending = home.pending().unwrap();
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert_eq!(pending[0].host, "newbox");
    let prev = home.preview_allow("newbox", &["src".into()]).unwrap();
    assert_eq!(prev.allow, Some(vec!["src".to_string()]));
    let approved = home.approve("newbox", &["src".into()]).unwrap();
    assert_eq!(approved.subscribed, vec!["src"]);
    let text = std::fs::read_to_string(bed.home_dirs.fleet()).unwrap();
    assert!(
        text.contains("[roots.src]"),
        "the hand edit survives: {text}"
    );
    assert!(!text.contains("[pending.newbox]"), "{text}");
}

#[test]
fn a_broken_hand_edit_refuses_requests_until_it_is_fixed() {
    let bed = two_subscriptions();
    let home = bed.home();
    assert_eq!(served(&home, &bed), ["work/a", "work/b"]);

    hand_edit(&bed, "[hosts.leaf]\n", "[hosts.leaf\n");
    let a = call(&home, "leaf", &bed.leaf_hd, HomeRequest::Subscriptions);
    let err = a.result.unwrap_err();
    assert!(err.contains("fleet.toml"), "names the file: {err}");
    // Still broken: the old cache must not quietly come back.
    let a = call(&home, "leaf", &bed.leaf_hd, HomeRequest::Subscriptions);
    assert!(a.result.is_err(), "the stale fleet was served");
    assert!(home.pending().is_err());

    hand_edit(
        &bed,
        "[hosts.leaf\n",
        "[hosts.leaf]\nallow = [\"work/b\"]\n",
    );
    assert_eq!(served(&home, &bed), ["work/b"], "recovers once fixed");
}

#[test]
fn home_s_own_writes_are_still_seen() {
    let bed = Bed::enrolled();
    let home = bed.home();
    let a = call(
        &home,
        "leaf",
        &bed.leaf_hd,
        HomeRequest::Unsubscribe {
            paths: vec![format!("{}/work/x", bed.leaf_hd.display())],
        },
    );
    assert!(a.result.is_ok(), "{:?}", a.result);
    let a = call(&home, "leaf", &bed.leaf_hd, HomeRequest::Subscriptions);
    let Ok(HomeReply::Subscriptions(v)) = a.result else {
        panic!()
    };
    assert_eq!(v.specs[0].except, vec!["work/x"]);
}
