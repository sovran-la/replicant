//! `[...]` classes: the only ones globset and rsync read alike.

/// The body of a `[...]` class, after the `[`. Positive, ASCII, no specials,
/// no range across `/`: the only classes globset and rsync both read
/// byte-for-byte the same.
pub(super) fn class(chars: &mut std::iter::Peekable<std::str::Chars>) -> Result<String, String> {
    let mut body = String::new();
    loop {
        match chars.next() {
            None => return Err("an unterminated `[` class".into()),
            Some(']') if !body.is_empty() => break,
            Some(c @ ('!' | '^')) if body.is_empty() => {
                return Err(format!(
                    "negated classes (`[{c}...]`) are refused: globset excludes one character, \
                     rsync one byte, so they disagree on non-ASCII names"
                ));
            }
            Some(']') => return Err("an empty `[]` class".into()),
            Some(c) if !c.is_ascii() || matches!(c, '/' | '\\' | '[') || c.is_ascii_control() => {
                return Err(format!("{c:?} inside a `[...]` class is not supported"));
            }
            Some(c) => body.push(c),
        }
    }
    let b = body.as_bytes();
    if b[0] == b'-' || b[b.len() - 1] == b'-' {
        return Err("a `-` at either end of a class is ambiguous; escape it outside one".into());
    }
    for i in 1..b.len() - 1 {
        if b[i] != b'-' {
            continue;
        }
        let (lo, hi) = (b[i - 1], b[i + 1]);
        if lo > hi || b.get(i + 2) == Some(&b'-') {
            return Err(format!("`[{body}]` has an invalid range"));
        }
        if lo <= b'/' && b'/' <= hi {
            return Err(format!(
                "`[{body}]` has a range that spans `/`: globset would match it across \
                 path components, rsync never does"
            ));
        }
    }
    Ok(body)
}
