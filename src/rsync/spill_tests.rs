use super::*;
use crate::exclude::Excludes;
use crate::marker::Direction;
use crate::rsync::{Runner, Transfer};
use crate::testutil;

fn args(rules: &[&str]) -> Vec<String> {
    let mut a = vec!["-a".to_string(), "--dry-run".to_string()];
    a.extend(rules.iter().map(|r| format!("{FLAG}{r}")));
    a.extend(["src/".to_string(), "dst/".to_string()]);
    a
}

#[test]
fn a_short_list_stays_on_the_command_line() {
    let a = args(&["/a", "/b/", "target"]);
    let (out, file) = spill(a.clone(), Path::new("/nonexistent"), LIMIT).unwrap();
    assert_eq!(out, a);
    assert!(file.is_none(), "nothing written, not even the scratch dir");
}

#[test]
fn a_long_list_moves_its_anchored_rules_to_a_private_file() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let scratch = tmp.path().join("cfg/tmp");
    let rules = [
        "target",
        "/a",
        "#lit",
        "/b/",
        "/line\nbreak",
        "/#hash",
        "/sp ace ",
    ];
    let (out, file) = spill(args(&rules), &scratch, 1).unwrap();
    let file = file.expect("spilled");
    assert_eq!(file.path().parent(), Some(scratch.as_path()));
    let m = std::fs::metadata(file.path()).unwrap();
    assert_eq!(m.permissions().mode() & 0o777, 0o600);
    assert_eq!(
        std::fs::read_to_string(file.path()).unwrap(),
        "/a\n/b/\n/#hash\n/sp ace \n"
    );
    let from = format!("--exclude-from={}", file.path().display());
    assert_eq!(
        out,
        vec![
            "-a",
            "--dry-run",
            "--exclude=target",
            &from,
            "--exclude=#lit",
            "--exclude=/line\nbreak",
            "src/",
            "dst/"
        ],
        "unanchored and line-breaking rules stay, the file takes the first one's place"
    );
    let path = file.path().to_path_buf();
    drop(file);
    assert!(!path.exists(), "removed on drop");
}

#[test]
fn the_limit_is_bytes_of_movable_rules() {
    let tmp = tempfile::tempdir().unwrap();
    let a = args(&["/abcd", "/ef", "unanchored-and-long"]);
    // "/abcd" + "/ef": 8 bytes; the unanchored rule never counts.
    assert!(
        spill(a.clone(), tmp.path(), 8).unwrap().1.is_none(),
        "at the limit"
    );
    assert!(spill(a, tmp.path(), 7).unwrap().1.is_some(), "past it");
}

#[test]
fn an_unusable_scratch_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("file");
    std::fs::write(&file, "").unwrap();
    assert!(spill(args(&["/a"]), &file, 0).is_err());
}

/// A real dry run with thousands of ignored paths: rsync reads the file and
/// leaves every listed path alone, and the file is gone afterwards.
#[test]
fn rsync_honors_a_spilled_list() {
    let tmp = tempfile::tempdir().unwrap();
    let (home, leaf) = (tmp.path().join("home"), tmp.path().join("leaf"));
    let mut ex = Excludes::for_root(&[]).unwrap();
    for i in 0..3000 {
        let name = format!("gen/ignored-file-number-{i:05}.pyc");
        testutil::write(&home.join(&name), "x");
        ex.push_exact(&name, false);
    }
    testutil::write(&home.join("gen/kept.py"), "x");
    testutil::write(&leaf.join("gen/leaf-ignored-[*].pyc"), "x");
    ex.push_exact("gen/leaf-ignored-[*].pyc", false);
    assert!(ex.rsync_args().iter().map(String::len).sum::<usize>() > LIMIT);
    let scratch = tmp.path().join("scratch");
    let runner = Runner {
        scratch: scratch.clone(),
        ..testutil::runner()
    };
    let it = Transfer {
        direction: Direction::Pull,
        leaf_path: leaf.clone(),
        home_path: home,
        home_host: None,
        is_file: false,
        excludes: ex,
        source_absent: false,
    }
    .dry_run(&runner)
    .unwrap();
    let touched: Vec<&str> = it.touched().collect();
    assert_eq!(touched, vec!["gen/kept.py"], "{:?}", it.changes);
    assert!(leaf.join("gen/leaf-ignored-[*].pyc").exists());
    assert_eq!(
        std::fs::read_dir(&scratch).unwrap().count(),
        0,
        "file removed"
    );
}
