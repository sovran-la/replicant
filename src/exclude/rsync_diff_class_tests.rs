//! Review P2: `[...]` ranges against the system rsync. A range that spans
//! `/` would match across components in globset and never in rsync, so it is
//! refused; one that does not, agrees.

use super::*;

#[test]
fn a_range_spanning_the_separator_is_refused_not_mismatched() {
    // ours would be {x/a.b, x/a/b, x/a0b}, rsync's {x/a.b, x/a0b}.
    let err = Excludes::new(&["x/a[+-0]b"]).unwrap_err().to_string();
    assert!(err.contains("spans `/`"), "{err}");
}

#[test]
fn ranges_beside_the_separator_agree_with_rsync() {
    let cases: &[(&str, &[&str])] = &[
        ("x/a[0-9]b", &["x/a0b"]),
        ("x/a[#-.]b", &["x/a.b"]),
        ("x/a[*-.0-9]b", &["x/a.b", "x/a0b"]),
    ];
    for (pat, want) in cases {
        let ex = Excludes::new(&[pat]).unwrap();
        assert_eq!(agree(&ex, pat), set(want), "{pat}: the expected set");
    }
}
