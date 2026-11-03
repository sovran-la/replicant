//! `[...]` classes: positive, ASCII, never across a `/`.

/// The body of a `[...]` class, after the `[`. Positive, ASCII, no specials,
/// no range across `/`: a class that means the same byte for byte whatever
/// the names hold (the subset fleet.toml has always accepted).
pub(super) fn class(chars: &mut std::iter::Peekable<std::str::Chars>) -> Result<String, String> {
    let mut body = String::new();
    loop {
        match chars.next() {
            None => return Err("an unterminated `[` class".into()),
            Some(']') if !body.is_empty() => break,
            Some(c @ ('!' | '^')) if body.is_empty() => {
                return Err(format!(
                    "negated classes (`[{c}...]`) are refused: on a non-ASCII name it is \
                     unclear whether they exclude one character or one byte"
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
                 path components"
            ));
        }
    }
    Ok(body)
}
