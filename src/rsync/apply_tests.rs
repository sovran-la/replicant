//! Real runs against the system rsync in tempdirs (openrsync on macOS), and
//! a script standing in for rsync where the arguments or a failure are the
//! point.

use super::*;
use crate::rsync::ChangeKind;
use crate::testutil;
use std::collections::BTreeMap;
use std::path::PathBuf;

fn runner(scratch: &Path) -> Runner {
    Runner::new(
        std::time::Duration::from_secs(60),
        super::super::Client::detect().unwrap(),
        scratch.to_path_buf(),
    )
}

fn transfer(direction: Direction, leaf: &Path, home: &Path) -> Transfer {
    Transfer {
        direction,
        leaf_path: leaf.into(),
        home_path: home.into(),
        home_host: None,
        is_file: false,
        excludes: Excludes::for_root(&["target".into()]).unwrap(),
        source_absent: false,
    }
}

/// Every entry under `root`: files with their bytes, links with their
/// target, directories as `dir/`.
fn tree(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(root).unwrap().display().to_string();
            let m = std::fs::symlink_metadata(&p).unwrap();
            if m.file_type().is_symlink() {
                let to = std::fs::read_link(&p).unwrap().display().to_string();
                out.insert(rel, format!("-> {to}"));
            } else if m.is_dir() {
                out.insert(format!("{rel}/"), String::new());
                stack.push(p);
            } else {
                out.insert(rel, std::fs::read_to_string(&p).unwrap_or_default());
            }
        }
    }
    out
}

fn sample(leaf: &Path, home: &Path) {
    testutil::write(&leaf.join("same"), "same\n");
    testutil::write(&leaf.join("changed"), "leaf's longer version\n");
    testutil::write(&leaf.join("sub/new"), "new\n");
    testutil::write(&leaf.join("target/junk"), "excluded on both sides\n");
    std::os::unix::fs::symlink("same", leaf.join("link")).unwrap();
    std::fs::create_dir_all(home).unwrap();
    assert!(
        std::process::Command::new("cp")
            .args(["-p"])
            .arg(leaf.join("same"))
            .arg(home.join("same"))
            .status()
            .unwrap()
            .success()
    );
    testutil::write(&home.join("changed"), "home's\n");
    testutil::write(&home.join("gone/old"), "deleted by the push\n");
    testutil::write(&home.join("target/keep"), "home's build output\n");
}

#[test]
fn apply_does_what_the_dry_run_said_and_nothing_else() {
    let tmp = tempfile::tempdir().unwrap();
    let (leaf, home) = (tmp.path().join("leaf"), tmp.path().join("home"));
    sample(&leaf, &home);
    let r = runner(&tmp.path().join("scratch"));
    let x = transfer(Direction::Push, &leaf, &home);
    let planned = x.dry_run(&r).unwrap();
    let ran = x.apply(&r).unwrap();
    assert!(ran.ok(), "{ran:?}");
    assert_eq!(ran.itemized.changes, planned.changes);
    let mut want = tree(&leaf);
    want.remove("target/");
    want.remove("target/junk");
    let mut got = tree(&home);
    assert_eq!(got.remove("target/"), Some(String::new()));
    assert_eq!(
        got.remove("target/keep").as_deref(),
        Some("home's build output\n"),
        "an excluded path is neither sent nor deleted"
    );
    assert_eq!(got, want);
    assert!(x.dry_run(&r).unwrap().is_empty(), "nothing left to do");
}

#[test]
fn apply_runs_the_dry_run_arguments_without_dry_run() {
    let tmp = tempfile::tempdir().unwrap();
    let argv = tmp.path().join("argv");
    let fake = testutil::script(
        tmp.path(),
        "fake-rsync",
        &format!("for a in \"$@\"; do echo \"$a\"; done > {}", argv.display()),
    );
    let mut r = runner(&tmp.path().join("scratch"));
    r.program = fake.display().to_string();
    for direction in [Direction::Pull, Direction::Push] {
        let mut x = transfer(direction, Path::new("/l/w"), Path::new("/h/w"));
        x.home_host = Some("thistledown".into());
        assert!(x.apply(&r).unwrap().ok());
        let got: Vec<String> = std::fs::read_to_string(&argv)
            .unwrap()
            .lines()
            .map(String::from)
            .collect();
        let mut want = x.args(&r, Path::new(""));
        want.retain(|a| a != "--dry-run");
        assert_eq!(got, want);
        assert!(got.contains(&"--delete".to_string()));
    }
}

#[test]
fn a_failed_run_keeps_what_rsync_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = testutil::script(
        tmp.path(),
        "fake-rsync",
        "echo '>f+++++++ half'; echo 'rsync: broken pipe' >&2; exit 23",
    );
    let mut r = runner(&tmp.path().join("scratch"));
    r.program = fake.display().to_string();
    let ran = transfer(Direction::Pull, Path::new("/l"), Path::new("/h"))
        .apply(&r)
        .unwrap();
    assert!(!ran.ok());
    assert_eq!(ran.code, Some(23));
    assert_eq!(ran.why(), "rsync exit 23");
    assert_eq!(ran.stderr, "rsync: broken pipe");
    assert_eq!(ran.itemized.changes.len(), 1);
    assert_eq!(ran.itemized.changes[0].path, "half");
    let vanished = Ran {
        code: Some(24),
        ..ran.clone()
    };
    assert!(!vanished.ok(), "files vanished is not a clean run");
    let aborted = Ran {
        stopped: Some(Stopped::Aborted),
        code: None,
        ..ran
    };
    assert!(aborted.why().contains("lease lost"));
}

#[path = "apply_list_tests.rs"]
mod list;
