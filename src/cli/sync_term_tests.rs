//! A scripted terminal for `sync`'s tests.

use std::io::BufRead;

/// A terminal: scripted input (a read error stands for Ctrl-C), captured
/// output.
pub(super) struct Script {
    pub(super) lines: Vec<std::io::Result<String>>,
}

impl std::io::Read for Script {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        unreachable!("read_line only")
    }
}

impl BufRead for Script {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        unreachable!("read_line only")
    }
    fn consume(&mut self, _: usize) {}
    fn read_line(&mut self, buf: &mut String) -> std::io::Result<usize> {
        if self.lines.is_empty() {
            return Ok(0);
        }
        let l = self.lines.remove(0)?;
        buf.push_str(&l);
        Ok(l.len())
    }
}

pub(super) fn ctrl_c() -> std::io::Result<String> {
    Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
}

pub(super) fn answer(s: &str) -> std::io::Result<String> {
    Ok(format!("{s}\n"))
}

/// Answers as the harness logs them: `rel=leaf` or `rel=home`, in order.
pub(super) fn answers(r: &[crate::decide::Resolution]) -> String {
    let each: Vec<String> = r
        .iter()
        .map(|r| {
            let side = match r.take {
                crate::decide::Take::Leaf => "leaf",
                crate::decide::Take::Home => "home",
            };
            format!("{}={side}", r.rel)
        })
        .collect();
    each.join(",")
}
