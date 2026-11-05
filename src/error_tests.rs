use super::*;
use crate::native::frame::{Frame, Kind};

/// What the other side's `Fail` says, once it has arrived.
fn arrived(sent: &Error) -> String {
    Frame::json(Kind::Fail, &sent.to_string())
        .unwrap()
        .expect(Kind::Ack)
        .unwrap_err()
        .to_string()
}

/// The re-review's nit: a `Transfer` error's text wrapped in another.
#[test]
fn a_transfer_error_from_the_other_side_says_transfer_once() {
    let e = Error::Transfer("\"ro/f.txt\": Permission denied".into());
    assert_eq!(arrived(&e), "transfer: \"ro/f.txt\": Permission denied");
    assert_eq!(arrived(&e), e.to_string());
}

#[test]
fn only_that_prefix_goes() {
    for (text, shown) in [
        ("disk full", "transfer: disk full"),
        ("io: disk full", "transfer: io: disk full"),
        ("a transfer: of sorts", "transfer: a transfer: of sorts"),
        ("transfer", "transfer: transfer"),
    ] {
        assert_eq!(Error::Transfer(text.into()).to_string(), shown);
    }
}

/// A note under an error keeps the error's own prefix, once (review A48).
#[test]
fn a_note_goes_under_the_error_and_doubles_no_prefix() {
    let e = Error::Noted(
        Box::new(Error::Remote("home at x:1 is unreachable".into())),
        "the saved list was replaced".into(),
    );
    assert_eq!(
        e.to_string(),
        "remote: home at x:1 is unreachable\nnote: the saved list was replaced"
    );
}
