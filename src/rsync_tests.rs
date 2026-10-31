use super::*;
use crate::testutil;
use std::collections::BTreeMap;

const OPENRSYNC: &str = "*deleting old/z
*deleting old
*deleting x2
cL+++++++ l
>f.s..... x
cd+++++++ sub/
>f+++++++ sub/y
.d..t.... ./
.f...p... perms.sh
";

const RSYNC3: &str = "*deleting   gone.txt
>f+++++++++ new.txt
>f.st...... changed.txt
.d..t...... dir/
cL+++++++++ link -> target
<f+++++++++ pushed.txt
";

#[test]
fn parses_openrsync_output() {
    let it = parse(OPENRSYNC);
    assert!(it.notes.is_empty(), "{:?}", it.notes);
    assert_eq!(it.deleted(), 2, "old/z and x2; old/ is a dir");
    assert_eq!(it.files_changed(), 3, "l, x, sub/y");
    let kinds: BTreeMap<_, _> = it
        .changes
        .iter()
        .map(|c| (c.path.as_str(), c.kind))
        .collect();
    assert_eq!(kinds["x"], ChangeKind::Update);
    assert_eq!(kinds["sub"], ChangeKind::Create);
    assert_eq!(kinds["perms.sh"], ChangeKind::Attr);
    assert!(
        !kinds.contains_key("."),
        "the root's own attribute line is dropped"
    );
    let touched: Vec<_> = it.touched().collect();
    assert_eq!(touched, vec!["old/z", "x2", "l", "x", "sub/y"]);
}

#[test]
fn parses_rsync3_output() {
    let it = parse(RSYNC3);
    assert_eq!(it.deleted(), 1);
    assert_eq!(it.files_changed(), 4);
    assert!(
        it.changes
            .iter()
            .any(|c| c.path == "gone.txt" && c.kind == ChangeKind::Delete)
    );
    assert!(
        it.changes.iter().any(|c| c.path == "link -> target"),
        "raw %n kept as printed"
    );
}

#[test]
fn unknown_lines_become_notes() {
    let it = parse("sending incremental file list\n>f+++++++ a\nweird\n");
    assert_eq!(it.changes.len(), 1);
    assert_eq!(it.notes, vec!["sending incremental file list", "weird"]);
}

#[test]
fn empty_means_no_file_changes() {
    assert!(parse("").is_empty());
    assert!(parse(".d..t.... sub/\n").is_empty());
    assert!(
        !parse(".f...p... a\n").is_empty(),
        "file attribute changes count"
    );
}

fn t(direction: Direction, leaf: &Path, home: &Path) -> Transfer {
    Transfer {
        direction,
        leaf_path: leaf.into(),
        home_path: home.into(),
        home_host: None,
        is_file: false,
        excludes: Excludes::for_root(&["target".into()]),
        source_absent: false,
    }
}

#[test]
fn args_always_dry_run_and_order_by_direction() {
    let leaf = Path::new("/l/work");
    let home = Path::new("/h/work");
    let mut x = t(Direction::Pull, leaf, home);
    x.home_host = Some("thistledown".into());
    let a = x.args(Path::new("/e"));
    assert!(a.contains(&"--dry-run".to_string()));
    assert!(a.contains(&"--delete".to_string()));
    assert!(a.contains(&"--exclude=target".to_string()));
    assert_eq!(&a[a.len() - 2..], ["thistledown:/h/work/", "/l/work/"]);
    x.direction = Direction::Push;
    let a = x.args(Path::new("/e"));
    assert_eq!(&a[a.len() - 2..], ["/l/work/", "thistledown:/h/work/"]);
    x.is_file = true;
    let a = x.args(Path::new("/e"));
    assert_eq!(&a[a.len() - 2..], ["/l/work", "thistledown:/h/work"]);
    x.is_file = false;
    x.source_absent = true;
    let a = x.args(Path::new("/e"));
    assert_eq!(
        &a[a.len() - 2..],
        ["/e/", "thistledown:/h/work/"],
        "empty stands in locally"
    );
}

/// Every file under `root` with its bytes, for before/after comparison.
fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(root).unwrap().display().to_string();
            if p.is_dir() {
                out.insert(format!("{rel}/"), vec![]);
                stack.push(p);
            } else {
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    out
}

#[test]
fn dry_run_against_a_temp_dir_pair_reports_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let leaf = tmp.path().join("leaf");
    testutil::write(&home.join("a.txt"), "home a");
    testutil::write(&home.join("sub/b.txt"), "home b");
    testutil::write(&home.join("target/build"), "excluded on source");
    testutil::write(&leaf.join("a.txt"), "leaf a, different");
    testutil::write(&leaf.join("only-leaf.txt"), "goes away on pull");
    testutil::write(&leaf.join("target/keep"), "excluded on target: protected");
    let before = (snapshot(&home), snapshot(&leaf));

    let it = t(Direction::Pull, &leaf, &home).dry_run().unwrap();
    assert_eq!(
        before,
        (snapshot(&home), snapshot(&leaf)),
        "a dry run writes nothing"
    );
    let touched: Vec<_> = it.touched().collect();
    assert!(touched.contains(&"a.txt"));
    assert!(touched.contains(&"sub/b.txt"));
    assert!(touched.contains(&"only-leaf.txt"), "deleted by --delete");
    assert!(
        !touched.iter().any(|p| p.starts_with("target")),
        "{touched:?}"
    );
    assert_eq!(it.deleted(), 1);
}

#[test]
fn push_dry_run_into_missing_target() {
    let tmp = tempfile::tempdir().unwrap();
    let leaf = tmp.path().join("leaf");
    testutil::write(&leaf.join("x"), "x");
    let home = tmp.path().join("home/does/not/exist");
    let it = t(Direction::Push, &leaf, &home).dry_run().unwrap();
    assert!(it.touched().any(|p| p == "x"), "{it:?}");
    assert!(!home.exists());
}

#[test]
fn absent_source_deletes_everything_on_target() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    testutil::write(&home.join("a"), "a");
    testutil::write(&home.join("d/b"), "b");
    let mut x = t(Direction::Push, &tmp.path().join("leaf-gone"), &home);
    x.source_absent = true;
    let it = x.dry_run().unwrap();
    assert_eq!(it.deleted(), 2);
    assert!(home.join("a").exists());
}

#[test]
fn absent_single_file_source_is_one_delete() {
    let mut x = t(Direction::Pull, Path::new("/l/f"), Path::new("/h/f"));
    x.is_file = true;
    x.source_absent = true;
    assert_eq!(x.dry_run().unwrap().deleted(), 1);
}

#[test]
fn single_file_unit_dry_run() {
    let tmp = tempfile::tempdir().unwrap();
    let h = tmp.path().join("settings.json");
    let l = tmp.path().join("leaf-settings.json");
    testutil::write(&h, "{\"a\":1}");
    testutil::write(&l, "{}");
    let mut x = t(Direction::Pull, &l, &h);
    x.is_file = true;
    let it = x.dry_run().unwrap();
    assert_eq!(it.files_changed(), 1, "{it:?}");
    assert_eq!(std::fs::read_to_string(&l).unwrap(), "{}");
}

#[test]
fn rsync_failure_is_an_error_naming_the_command() {
    let x = t(
        Direction::Pull,
        Path::new("/nonexistent/leaf"),
        Path::new("/nonexistent/home"),
    );
    let err = x.dry_run().unwrap_err().to_string();
    assert!(err.contains("rsync") && err.contains("--dry-run"), "{err}");
}
