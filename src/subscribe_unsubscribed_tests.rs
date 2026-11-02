//! Review F5: `unsubscribe` records what it removed, so home can tell a leaf
//! which out-of-bounds baselines were given up on purpose; `subscribe`
//! forgets the record for whatever it restores.

use super::*;

fn sub(p: &Path, host: &str, raw: &[&str]) {
    subscribe(&load(p), p, host, Path::new(MAC), &s(raw)).unwrap();
}

fn unsub(p: &Path, host: &str, raw: &[&str]) -> UnsubscribeOutcome {
    unsubscribe(&load(p), p, host, Path::new(MAC), &s(raw)).unwrap()
}

#[test]
fn removing_a_subscription_records_it_and_a_carve_out_does_not() {
    let (_t, p) = fixture();
    sub(&p, "birchbook", &["~/work/a", "~/work/b", "~/.claude"]);
    unsub(&p, "birchbook", &["~/work/b"]);
    let got = load(&p).subscription("birchbook");
    assert_eq!(got.paths, s(&["work/a", ".claude"]));
    assert_eq!(got.unsubscribed, s(&["work/b"]));

    // A carve-out is recorded in `except`, which says so already.
    unsub(&p, "birchbook", &["~/work/a/x"]);
    let got = load(&p).subscription("birchbook");
    assert_eq!(got.except, s(&["work/a/x"]));
    assert_eq!(got.unsubscribed, s(&["work/b"]));
}

#[test]
fn unsubscribing_above_records_each_removed_subscription_collapsed() {
    let (_t, p) = fixture();
    sub(&p, "birchbook", &["~/work/a", "~/work/b"]);
    let out = unsub(&p, "birchbook", &["~/work"]);
    assert_eq!(out.removed, s(&["work/a", "work/b"]));
    assert_eq!(
        load(&p).subscription("birchbook").unsubscribed,
        s(&["work/a", "work/b"])
    );
    // Removing the parent later absorbs the children's records.
    sub(&p, "birchbook", &["~/work"]);
    unsub(&p, "birchbook", &["~/work"]);
    assert_eq!(
        load(&p).subscription("birchbook").unsubscribed,
        s(&["work"])
    );
}

#[test]
fn resubscribing_forgets_the_record_for_what_it_restores_only() {
    let (_t, p) = fixture();
    sub(&p, "birchbook", &["~/work/a", "~/work/b", "~/.claude"]);
    unsub(&p, "birchbook", &["~/work/a", "~/work/b", "~/.claude"]);
    sub(&p, "birchbook", &["~/work"]);
    let got = load(&p).subscription("birchbook");
    assert_eq!(got.paths, s(&["work"]));
    assert_eq!(got.unsubscribed, s(&[".claude"]), "{got:?}");

    // A subscription inside an unsubscribed one leaves the record standing:
    // the rest of it is still given up.
    unsub(&p, "birchbook", &["~/work"]);
    sub(&p, "birchbook", &["~/work/a"]);
    let got = load(&p).subscription("birchbook");
    assert_eq!(got.unsubscribed, s(&[".claude", "work"]), "{got:?}");
}

#[test]
fn approve_starts_with_no_record() {
    let (_t, p) = fixture();
    sub(&p, "spruce", &["~/work"]);
    approve(&load(&p), &p, "spruce", &[]).unwrap();
    assert!(load(&p).subscription("spruce").unsubscribed.is_empty());
}
