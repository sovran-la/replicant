//! What `sync` did: one line per unit applied, then how it ended. Red for
//! anything that needs a human; the end line always says whether anything
//! was written and what the next `sync` will do.

use super::Style;
use crate::plan::apply::{Outcome, SyncReport};
use std::fmt::Write;

pub fn synced(r: &SyncReport, st: Style) -> String {
    let mut s = String::new();
    if !r.applied.is_empty() {
        s.push_str("applied:\n");
    }
    for a in &r.applied {
        let resumed = if a.resumed { ", resumed" } else { "" };
        let _ = writeln!(
            s,
            "  {:<15} {}  {} files changed, {} deleted{resumed}",
            a.action, a.rel, a.files_changed, a.deleted
        );
    }
    let n = r.applied.len();
    let units = if n == 1 { "unit" } else { "units" };
    match &r.outcome {
        Outcome::Done if n == 0 => s.push_str("nothing to apply\n"),
        Outcome::Done => {
            let _ = writeln!(s, "done: {n} {units} applied");
        }
        Outcome::Blocked { why } => {
            let _ = writeln!(s, "{}", st.red("blocked; nothing written:"));
            for w in why {
                let _ = writeln!(s, "  {}", st.red(w));
            }
        }
        Outcome::Changed => {
            let _ = writeln!(
                s,
                "{}",
                st.red(
                    "the plan changed after it was shown; nothing written. Run `replicant sync` \
                     again to see the new one."
                )
            );
        }
        Outcome::Interrupted { rel, why, .. } => {
            let _ = writeln!(
                s,
                "{}",
                st.red(&format!(
                    "interrupted at {rel}: {why}. {n} {units} before it are done; its marker \
                     stays and the next `replicant sync` resumes it; nothing after it was touched."
                ))
            );
        }
        Outcome::Failed { rel, why } => {
            let _ = writeln!(
                s,
                "{}",
                st.red(&format!(
                    "failed at {rel}: {why}. {n} {units} before it are done; any marker it left \
                     stays and the next `replicant sync` resumes it."
                ))
            );
        }
    }
    s
}
