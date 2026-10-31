//! Reading rsync's itemized output.

use super::{Change, ChangeKind, Itemized};

/// Parse `--itemize-changes --out-format=%i %n` output.
pub fn parse(text: &str) -> Itemized {
    let mut out = Itemized::default();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        if let Some(rest) = line.strip_prefix("*deleting") {
            let name = rest.trim_start();
            out.changes.push(Change {
                kind: ChangeKind::Delete,
                is_dir: name.ends_with('/'),
                path: name.trim_end_matches('/').to_string(),
            });
            continue;
        }
        let Some((code, name)) = line.split_once(' ') else {
            out.notes.push(line.to_string());
            continue;
        };
        let b = code.as_bytes();
        let shaped = b.len() >= 9 && b"<>ch.".contains(&b[0]) && b"fdLDS".contains(&b[1]);
        if !shaped {
            out.notes.push(line.to_string());
            continue;
        }
        let kind = if b[2..].iter().all(|c| *c == b'+') {
            ChangeKind::Create
        } else if b[0] == b'.' {
            ChangeKind::Attr
        } else {
            ChangeKind::Update
        };
        let name = name.strip_suffix('/').unwrap_or(name);
        // The transfer root itself: its creation or attributes are not a change
        // anybody needs to read.
        if name == "." || name.is_empty() {
            continue;
        }
        out.changes.push(Change {
            kind,
            is_dir: b[1] == b'd',
            path: name.to_string(),
        });
    }
    // openrsync prints a deleted directory without its trailing slash, so a
    // deletion is a directory when other deletions lie beneath it.
    let deleted: Vec<String> = out
        .changes
        .iter()
        .filter(|c| c.kind == ChangeKind::Delete)
        .map(|c| format!("{}/", c.path))
        .collect();
    for c in out
        .changes
        .iter_mut()
        .filter(|c| c.kind == ChangeKind::Delete && !c.is_dir)
    {
        let prefix = format!("{}/", c.path);
        c.is_dir = deleted
            .iter()
            .any(|d| d.len() > prefix.len() && d.starts_with(&prefix));
    }
    out
}
