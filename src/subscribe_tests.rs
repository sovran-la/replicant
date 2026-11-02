use super::*;
use std::path::PathBuf;

const FLEET: &str = r#"
[daemon]
name = "thistledown"
home_dir = "/home/morgan"

[roots.work]
path = "/home/morgan/work"

[roots.claude]
path = "/home/morgan/.claude"

[hosts.birchbook]
home_dir = "/Users/morgan"

[hosts.twinoaks]
home_dir = "/Users/morgan"
allow = ["work/tundra", "work/sequoia"]
"#;

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("fleet.toml");
    std::fs::write(&p, FLEET).unwrap();
    (tmp, p)
}

fn load(p: &Path) -> Fleet {
    Fleet::load(p).unwrap()
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

const MAC: &str = "/Users/morgan";

#[test]
fn unknown_host_becomes_pending_and_is_served_nothing() {
    let (_t, p) = fixture();
    let out = subscribe(
        &load(&p),
        &p,
        "spruce",
        Path::new("/home/morgan"),
        &s(&["/home/morgan/work/rust"]),
    )
    .unwrap();
    assert!(!out.enrolled);
    assert_eq!(out.requested, s(&["work/rust"]));
    let f = load(&p);
    assert_eq!(f.pending["spruce"].paths, s(&["work/rust"]));
    assert_eq!(f.pending["spruce"].home_dir, PathBuf::from("/home/morgan"));
    assert!(f.pending["spruce"].requested_at > 0);
    assert!(!f.hosts.contains_key("spruce"));
    assert!(f.subscription("spruce").paths.is_empty(), "nothing served");

    let again = subscribe(&f, &p, "spruce", Path::new("/home/morgan"), &s(&["~/work"])).unwrap();
    assert_eq!(again.paths, s(&["work"]));
    assert_eq!(again.absorbed, s(&["work/rust"]));
}

#[test]
fn capped_host_subscribing_outside_its_cap_is_refused_loudly_and_nothing_is_written() {
    let (_t, p) = fixture();
    let before = std::fs::read_to_string(&p).unwrap();
    let err = subscribe(
        &load(&p),
        &p,
        "twinoaks",
        Path::new(MAC),
        &s(&["~/work/tundra", "~/work"]),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("capped")
            && err.contains("refusing work")
            && !err.contains("refusing work/tundra"),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(&p).unwrap(), before);
}

#[test]
fn capped_host_may_narrow_within_its_cap() {
    let (_t, p) = fixture();
    let out = subscribe(
        &load(&p),
        &p,
        "twinoaks",
        Path::new(MAC),
        &s(&["/Users/morgan/work/tundra/beacon"]),
    )
    .unwrap();
    assert!(out.enrolled);
    assert_eq!(
        load(&p).subscription("twinoaks").paths,
        s(&["work/tundra/beacon"])
    );
}

#[test]
fn enrolled_host_merges_and_collapses() {
    let (_t, p) = fixture();
    subscribe(
        &load(&p),
        &p,
        "birchbook",
        Path::new(MAC),
        &s(&["~/work/tundra", "~/.claude/skills"]),
    )
    .unwrap();
    let out = subscribe(&load(&p), &p, "birchbook", Path::new(MAC), &s(&["~/work"])).unwrap();
    // Existing paths keep their place; the new broader one is appended.
    assert_eq!(out.paths, s(&[".claude/skills", "work"]));
    assert_eq!(out.absorbed, s(&["work/tundra"]));
}

#[test]
fn enrolled_host_uses_recorded_home_dir_not_the_claimed_one() {
    let (_t, p) = fixture();
    // birchbook is recorded with /Users/morgan; a request claiming /home/x
    // still normalizes /Users/morgan/... paths.
    let out = subscribe(
        &load(&p),
        &p,
        "birchbook",
        Path::new("/home/x"),
        &s(&["/Users/morgan/work/a"]),
    )
    .unwrap();
    assert_eq!(out.requested, s(&["work/a"]));
}

#[test]
fn paths_outside_roots_are_refused() {
    let (_t, p) = fixture();
    let err = subscribe(
        &load(&p),
        &p,
        "birchbook",
        Path::new(MAC),
        &s(&["~/Downloads"]),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("roots: .claude, work"), "{err}");
    assert!(subscribe(&load(&p), &p, "birchbook", Path::new(MAC), &[]).is_err());
}

#[test]
fn unsubscribe_whole_carve_above_and_unknown() {
    let (_t, p) = fixture();
    subscribe(
        &load(&p),
        &p,
        "birchbook",
        Path::new(MAC),
        &s(&["~/work", "~/.claude/skills"]),
    )
    .unwrap();

    let out = unsubscribe(
        &load(&p),
        &p,
        "birchbook",
        Path::new(MAC),
        &s(&["~/work/rust"]),
    )
    .unwrap();
    assert_eq!(out.carved, s(&["work/rust"]));
    assert_eq!(load(&p).subscription("birchbook").except, s(&["work/rust"]));

    // Carving inside an existing carve-out is a no-op, not a duplicate.
    let out = unsubscribe(
        &load(&p),
        &p,
        "birchbook",
        Path::new(MAC),
        &s(&["~/work/rust/x"]),
    )
    .unwrap();
    assert!(out.carved.is_empty());

    // Re-subscribing restores it.
    subscribe(
        &load(&p),
        &p,
        "birchbook",
        Path::new(MAC),
        &s(&["~/work/rust"]),
    )
    .unwrap();
    assert!(load(&p).subscription("birchbook").except.is_empty());

    let out = unsubscribe(
        &load(&p),
        &p,
        "birchbook",
        Path::new(MAC),
        &s(&["~/.claude"]),
    )
    .unwrap();
    assert_eq!(
        out.removed,
        s(&[".claude/skills"]),
        "a path above removes what is beneath"
    );

    let err = unsubscribe(
        &load(&p),
        &p,
        "birchbook",
        Path::new(MAC),
        &s(&["~/.claude/x"]),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("not subscribed"), "{err}");

    let out = unsubscribe(&load(&p), &p, "birchbook", Path::new(MAC), &s(&["~/work"])).unwrap();
    assert_eq!(out.removed, s(&["work"]));
    assert!(load(&p).subscription("birchbook").paths.is_empty());
}

#[test]
fn unsubscribe_while_pending_trims_the_request() {
    let (_t, p) = fixture();
    subscribe(
        &load(&p),
        &p,
        "spruce",
        Path::new("/home/morgan"),
        &s(&["~/work/a", "~/work/b"]),
    )
    .unwrap();
    let out = unsubscribe(
        &load(&p),
        &p,
        "spruce",
        Path::new("/home/morgan"),
        &s(&["~/work/a"]),
    )
    .unwrap();
    assert_eq!(out.removed, s(&["work/a"]));
    assert_eq!(load(&p).pending["spruce"].paths, s(&["work/b"]));
    unsubscribe(
        &load(&p),
        &p,
        "spruce",
        Path::new("/home/morgan"),
        &s(&["~/work/b"]),
    )
    .unwrap();
    assert!(load(&p).pending.is_empty());
    assert!(unsubscribe(&load(&p), &p, "nobody", Path::new(MAC), &s(&["~/work"])).is_err());
}

#[path = "subscribe_approve_tests.rs"]
mod more;

#[path = "subscribe_unsubscribed_tests.rs"]
mod unsubscribed;
