//! approve and its preview: the pending-host flow on home.

use super::*;

#[test]
fn pending_host_flow_preview_then_approve_with_cap() {
    let (_t, p) = fixture();
    subscribe(
        &load(&p),
        &p,
        "newbox",
        Path::new(MAC),
        &s(&["~/work/tundra", "~/work/sequoia", "~/work/rust"]),
    )
    .unwrap();
    let f = load(&p);
    // Typed on home from muscle memory: mixed forms, one string with spaces.
    let prev = preview_allow(
        &f,
        "newbox",
        &s(&["/Users/morgan/work/sequoia ~/work/tundra"]),
    )
    .unwrap();
    assert_eq!(prev.allow, Some(s(&["work/sequoia", "work/tundra"])));
    assert_eq!(prev.keep, s(&["work/tundra", "work/sequoia"]));
    assert_eq!(prev.drop, s(&["work/rust"]));
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        std::fs::read_to_string(&p).unwrap(),
        "preview writes nothing"
    );

    let ok = approve(
        &f,
        &p,
        "newbox",
        &s(&["/Users/morgan/work/sequoia ~/work/tundra"]),
    )
    .unwrap();
    assert_eq!(ok.dropped, s(&["work/rust"]));
    let f = load(&p);
    assert!(f.pending.is_empty());
    assert_eq!(f.hosts["newbox"].home_dir, PathBuf::from(MAC));
    assert_eq!(
        f.hosts["newbox"].allow,
        Some(s(&["work/sequoia", "work/tundra"]))
    );
    assert_eq!(
        f.subscription("newbox").paths,
        s(&["work/tundra", "work/sequoia"])
    );

    // And the cap now binds future subscribes.
    assert!(subscribe(&f, &p, "newbox", Path::new(MAC), &s(&["~/work/rust"])).is_err());
}

#[test]
fn approve_with_empty_allow_means_no_cap() {
    let (_t, p) = fixture();
    subscribe(&load(&p), &p, "open", Path::new(MAC), &s(&["~/work"])).unwrap();
    let ok = approve(&load(&p), &p, "open", &s(&[""])).unwrap();
    assert_eq!(ok.allow, None);
    assert_eq!(load(&p).hosts["open"].allow, None);
    assert_eq!(load(&p).subscription("open").paths, s(&["work"]));
}

#[test]
fn approve_or_preview_of_a_non_pending_host_is_refused() {
    let (_t, p) = fixture();
    assert!(preview_allow(&load(&p), "birchbook", &[]).is_err());
    assert!(approve(&load(&p), &p, "ghost", &[]).is_err());
}

#[test]
fn bad_allow_is_refused_before_anything_is_written() {
    let (_t, p) = fixture();
    subscribe(&load(&p), &p, "newbox", Path::new(MAC), &s(&["~/work"])).unwrap();
    let before = std::fs::read_to_string(&p).unwrap();
    assert!(approve(&load(&p), &p, "newbox", &s(&["~/work/../etc"])).is_err());
    assert_eq!(std::fs::read_to_string(&p).unwrap(), before);
}

#[test]
fn approve_is_one_write_a_failing_step_leaves_the_file_untouched() {
    let (_t, p) = fixture();
    subscribe(&load(&p), &p, "newbox", Path::new(MAC), &s(&["~/work"])).unwrap();
    let f = load(&p);
    // A hand edit after the daemon loaded the fleet: the subscription step
    // cannot succeed, though the host step before it can.
    let broken = format!(
        "subscriptions = 3\n{}",
        std::fs::read_to_string(&p).unwrap()
    );
    std::fs::write(&p, &broken).unwrap();
    assert!(approve(&f, &p, "newbox", &s(&[""])).is_err());
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        broken,
        "not enrolled and still pending: nothing half-written"
    );
}

#[test]
fn approve_writes_host_subscription_and_clears_pending_together() {
    let (_t, p) = fixture();
    subscribe(&load(&p), &p, "newbox", Path::new(MAC), &s(&["~/work"])).unwrap();
    approve(&load(&p), &p, "newbox", &s(&["~/work"])).unwrap();
    let text = std::fs::read_to_string(&p).unwrap();
    assert!(text.contains("[hosts.newbox]"), "{text}");
    assert!(text.contains("[subscriptions.newbox]"), "{text}");
    assert!(!text.contains("[pending"), "{text}");
    assert!(text.contains("[hosts.twinoaks]"), "others kept: {text}");
}
