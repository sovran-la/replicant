//! A unit's live counters: `12,345/70,713 files  3.2 GB/14.1 GB`.

use crate::plan::apply::ProgressEvent;

/// `1234567` as `1,234,567`.
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Bytes in decimal units, one decimal past a kilobyte: `512 B`, `3.2 GB`.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["kB", "MB", "GB", "TB", "PB"];
    if n < 1000 {
        return format!("{n} B");
    }
    let mut v = n as f64 / 1000.0;
    let mut u = 0;
    while v >= 999.95 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    format!("{v:.1} {}", UNITS[u])
}

/// The counters part of a unit's line.
pub fn counters(files_done: u64, files_total: u64, bytes_done: u64, bytes_total: u64) -> String {
    format!(
        "{}/{} files  {}/{}",
        thousands(files_done),
        thousands(files_total),
        bytes(bytes_done),
        bytes(bytes_total)
    )
}

/// The unit's line so far, from the start of the terminal line: what
/// `UnitStarted` printed, and the counters. Empty for any other event.
pub fn redraw(e: &ProgressEvent) -> String {
    match e {
        ProgressEvent::UnitProgress {
            rel,
            action,
            index,
            total,
            files_done,
            files_total,
            bytes_done,
            bytes_total,
        } => format!(
            "\r\x1b[2K  [{index}/{total}] {action}  {rel} … {}",
            counters(*files_done, *files_total, *bytes_done, *bytes_total)
        ),
        _ => String::new(),
    }
}

/// What a sync's stream of events prints, event by event: the unit lines,
/// counters redrawn over the running unit's line at a terminal (and left
/// out without one), or one JSON object per event.
pub struct Lines {
    json: bool,
    terminal: bool,
    style: super::Style,
    /// The running unit's start, and whether counters were drawn over it.
    start: String,
    drawn: bool,
}

impl Lines {
    pub fn new(json: bool, terminal: bool, style: super::Style) -> Self {
        Self {
            json,
            terminal,
            style,
            start: String::new(),
            drawn: false,
        }
    }

    /// What to print for `e`; empty for nothing.
    pub fn line(&mut self, e: &ProgressEvent) -> String {
        if self.json {
            return serde_json::to_string(e)
                .map(|j| j + "\n")
                .unwrap_or_default();
        }
        match e {
            ProgressEvent::UnitProgress { .. } => {
                self.drawn = self.terminal;
                match self.terminal {
                    true => redraw(e),
                    false => String::new(),
                }
            }
            ProgressEvent::UnitStarted { .. } => {
                self.start = super::progress(e, self.style);
                self.drawn = false;
                self.start.clone()
            }
            ProgressEvent::UnitDone { .. } if self.drawn => {
                self.drawn = false;
                format!("\r\x1b[2K{}{}", self.start, super::progress(e, self.style))
            }
            ProgressEvent::UnitDone { .. } => super::progress(e, self.style),
        }
    }
}

#[cfg(test)]
#[path = "render_counts_tests.rs"]
mod tests;
