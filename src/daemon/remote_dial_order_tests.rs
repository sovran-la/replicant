//! The dial order within one name, the candidate order on a redial, and
//! the line that names what was skipped: pure functions, no sockets.

use super::*;

fn sa(s: &str) -> SocketAddr {
    s.parse().unwrap()
}

#[test]
fn candidates_put_the_ip_that_answered_first() {
    let (a, b, c) = (sa("10.0.0.1:1"), sa("10.0.0.2:1"), sa("[2001:db8::1]:1"));
    // No memory: dial order as resolved, v4 first.
    assert_eq!(candidates(vec![c, a, b], None), vec![a, b, c]);
    // The IP that answered last time moves to the front, even a v6 one.
    assert_eq!(candidates(vec![c, a, b], Some(b)), vec![b, a, c]);
    assert_eq!(candidates(vec![c, a, b], Some(c)), vec![c, a, b]);
    // One the name no longer resolves to is ignored, not invented.
    assert_eq!(candidates(vec![a, b], Some(sa("10.0.0.9:1"))), vec![a, b]);
}

#[test]
fn dial_order_puts_ipv4_first_and_drops_unscoped_link_local() {
    // The shape mDNS gives back for `host.local`: link-local v6 first, v4 after.
    let got = dial_order(vec![
        sa("[fe80::a2c7:5e8a:89be:46a]:7750"),
        sa("192.168.10.199:7750"),
    ]);
    assert_eq!(got, vec![sa("192.168.10.199:7750")]);

    // Global v6 is kept, but after every v4; relative order within a family
    // is preserved.
    let got = dial_order(vec![
        sa("[2001:db8::1]:1"),
        sa("10.0.0.1:1"),
        sa("[fe80::1]:1"),
        sa("10.0.0.2:1"),
        sa("[2001:db8::2]:1"),
    ]);
    assert_eq!(
        got,
        vec![
            sa("10.0.0.1:1"),
            sa("10.0.0.2:1"),
            sa("[2001:db8::1]:1"),
            sa("[2001:db8::2]:1"),
        ]
    );

    // A link-local address WITH a scope is routable and stays.
    let scoped = SocketAddr::V6(std::net::SocketAddrV6::new(
        "fe80::1".parse().unwrap(),
        7750,
        0,
        4,
    ));
    assert_eq!(dial_order(vec![scoped]), vec![scoped]);

    // Loopback v6 and the whole fe80::/10 block are classified by prefix.
    assert_eq!(dial_order(vec![sa("[::1]:1")]), vec![sa("[::1]:1")]);
    assert_eq!(dial_order(vec![sa("[febf::1]:1")]), vec![]);
    assert_eq!(dial_order(vec![sa("[fec0::1]:1")]), vec![sa("[fec0::1]:1")]);
    assert_eq!(dial_order(vec![]), vec![]);
}
#[test]
fn the_skipped_line_names_each_miss_and_is_absent_without_one() {
    assert_eq!(skipped_line("b:1", &[]), None);
    let (a, c) = ("a:1".to_string(), "c:1".to_string());
    let misses = vec![
        (&a, Miss::Unreachable("Connection refused".into())),
        (&c, Miss::Refused("bad signature".into())),
    ];
    assert_eq!(
        skipped_line("b:1", &misses).as_deref(),
        Some("home answered at b:1; skipped a:1 (Connection refused), c:1 (bad signature)")
    );
}
