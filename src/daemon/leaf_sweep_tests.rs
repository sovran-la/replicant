use super::*;
use crate::marker::Direction;
use crate::record::RECORD_VERSION;
use crate::state::UnitState;
use crate::testutil;

const DAY: u64 = 86_400;
const NOW: u64 = 1_800_000_000;

struct Bed {
    _tmp: tempfile::TempDir,
    backup: std::path::PathBuf,
    state: std::path::PathBuf,
    journal: Journal,
}

fn bed() -> Bed {
    let tmp = tempfile::tempdir().unwrap();
    let backup = tmp.path().join("backup");
    let state = tmp.path().join("state");
    let journal = Journal::at(tmp.path().join("journal.jsonl"));
    Bed {
        _tmp: tmp,
        backup,
        state,
        journal,
    }
}

impl Bed {
    /// A resolved pull's run, `age` seconds before NOW, holding one file.
    fn run(&self, age: u64, rel: &str) -> std::path::PathBuf {
        let dir = self
            .backup
            .join(RESOLVED)
            .join(timefmt::compact(NOW - age))
            .join(rel);
        testutil::write(&dir.join("kept.txt"), "what the pull replaced\n");
        dir
    }

    fn sweep(&self) -> usize {
        sweep(&self.backup, &self.state, &self.journal, "leaf", NOW).unwrap()
    }

    fn lines(&self) -> Vec<String> {
        self.journal
            .read(&Default::default())
            .unwrap()
            .into_iter()
            .map(|l| l.detail)
            .collect()
    }
}

#[test]
fn nothing_to_sweep_is_not_an_error_and_journals_nothing() {
    let b = bed();
    assert_eq!(b.sweep(), 0);
    assert!(b.lines().is_empty());
}

#[test]
fn runs_older_than_the_keep_go_and_newer_ones_stay() {
    let b = bed();
    let old = b.run((KEEP_DAYS + 1) * DAY, "work/a");
    let edge = b.run(KEEP_DAYS * DAY - 1, "work/b");
    let new = b.run(DAY, "work/c");
    assert_eq!(b.sweep(), 1);
    assert!(!old.exists());
    assert!(
        !old.parent().unwrap().parent().unwrap().exists(),
        "the whole run"
    );
    assert!(edge.exists() && new.exists());
    assert_eq!(
        b.lines(),
        vec![format!(
            "removed 1 resolved-pull backups older than {KEEP_DAYS} days"
        )]
    );
}

/// An interrupted resolved pull resumes into the same run: its backup is
/// the only copy of what it already replaced.
#[test]
fn a_run_an_open_marker_names_is_never_swept() {
    let b = bed();
    let held = b.run((KEEP_DAYS + 30) * DAY, "work/r");
    marker::store(&b.state)
        .put(&Marker {
            version: RECORD_VERSION,
            rel: "work/r".into(),
            direction: Direction::Pull,
            host: "leaf".into(),
            source_state: UnitState::Present("s".into()),
            started_at: NOW - (KEEP_DAYS + 30) * DAY,
            push: None,
            backup: Some(held.clone()),
        })
        .unwrap();
    assert_eq!(b.sweep(), 0);
    assert!(held.join("kept.txt").exists());
}

#[test]
fn what_is_not_a_run_is_left_alone() {
    let b = bed();
    let root = b.backup.join(RESOLVED);
    testutil::write(&root.join("README"), "a file, not a run\n");
    testutil::write(
        &root.join("not-a-time/x"),
        "a directory not named for one\n",
    );
    assert_eq!(b.sweep(), 0);
    assert!(root.join("README").exists());
    assert!(root.join("not-a-time/x").exists());
}
