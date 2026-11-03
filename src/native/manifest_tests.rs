use super::*;
use crate::testutil::{self, write};
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, SystemTime};

pub(crate) fn set_mtime(p: &Path, secs: u64, ns: u32) {
    let f = fs::File::options().write(true).open(p).unwrap();
    f.set_times(
        fs::FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::new(secs, ns)),
    )
    .unwrap();
}

fn rels(m: &Manifest) -> Vec<&str> {
    m.entries.iter().map(|e| e.rel.as_str()).collect()
}

fn none() -> Excludes {
    Excludes::for_root(&[]).unwrap()
}

#[test]
fn lists_files_dirs_and_links_with_their_metadata() {
    let t = tempfile::tempdir().unwrap();
    let r = t.path();
    write(&r.join("a/b.txt"), "hello");
    fs::set_permissions(r.join("a/b.txt"), fs::Permissions::from_mode(0o640)).unwrap();
    set_mtime(&r.join("a/b.txt"), 1_700_000_000, 123_456_789);
    fs::set_permissions(r.join("a"), fs::Permissions::from_mode(0o750)).unwrap();
    std::os::unix::fs::symlink("a/b.txt", r.join("link")).unwrap();

    let m = Manifest::walk(r, &none(), false).unwrap();
    assert_eq!(rels(&m), ["a", "a/b.txt", "link"]);
    let f = m.get("a/b.txt").unwrap();
    assert_eq!(f.kind, EntryKind::File);
    assert_eq!(f.size, 5);
    assert_eq!(f.mtime_ns, 1_700_000_000_123_456_789);
    assert_eq!(f.mode, 0o640);
    let d = m.get("a").unwrap();
    assert_eq!(
        (d.kind, d.mode, d.size, d.mtime_ns),
        (EntryKind::Dir, 0o750, 0, 0)
    );
    let l = m.get("link").unwrap();
    assert_eq!(l.kind, EntryKind::Symlink);
    assert_eq!(l.link.as_deref(), Some("a/b.txt"));
    assert_eq!((l.size, l.mtime_ns), (0, 0));
    assert_eq!(m.totals(), (2, 5));
}

#[test]
fn symlinks_inside_the_unit_are_never_followed() {
    let t = tempfile::tempdir().unwrap();
    let outside = t.path().join("outside");
    write(&outside.join("secret"), "x");
    let r = t.path().join("unit");
    fs::create_dir_all(&r).unwrap();
    std::os::unix::fs::symlink(&outside, r.join("out")).unwrap();
    let m = Manifest::walk(&r, &none(), false).unwrap();
    assert_eq!(rels(&m), ["out"]);
    assert_eq!(m.entries[0].kind, EntryKind::Symlink);
}

#[test]
fn root_excludes_are_applied_and_excluded_dirs_not_descended() {
    let t = tempfile::tempdir().unwrap();
    let r = t.path();
    write(&r.join("src/main.rs"), "x");
    write(&r.join("target/debug/out"), "x");
    write(&r.join("deep/target/x"), "x");
    write(&r.join("agents/state/db"), "x");
    write(&r.join("agents/types/t"), "x");
    write(&r.join(".git/index.lock"), "");
    write(&r.join(".git/HEAD"), "ref: refs/heads/main\n");
    let ex = Excludes::for_root(&["target".into(), "agents/state".into()]).unwrap();
    let m = Manifest::walk(r, &ex, false).unwrap();
    assert_eq!(
        rels(&m),
        [
            ".git",
            ".git/HEAD",
            "agents",
            "agents/types",
            "agents/types/t",
            "deep",
            "src",
            "src/main.rs"
        ]
    );
}

#[test]
fn git_ignored_paths_are_left_out_negations_and_tracked_paths_kept() {
    let t = tempfile::tempdir().unwrap();
    let r = testutil::repo(&t.path().join("r"));
    write(&r.join(".gitignore"), "*.log\n!keep.log\nbuild/\n");
    write(&r.join("a.log"), "x");
    write(&r.join("keep.log"), "x");
    write(&r.join("build/out.o"), "x");
    write(&r.join("sub/deep.log"), "x");
    write(&r.join("tracked.log"), "x");
    testutil::git(&r, &["add", "-f", "tracked.log", ".gitignore"]);
    testutil::git(&r, &["commit", "-qm", "t"]);

    let ex = Excludes::for_root(&[]).unwrap();
    let ignored = crate::repos::ignored(&r).unwrap();
    let ex = crate::units::with_ignored(ex, &ignored);
    let m = Manifest::walk(&r, &ex, false).unwrap();
    let names: Vec<&str> = rels(&m)
        .into_iter()
        .filter(|p| !p.starts_with(".git/"))
        .collect();
    assert!(
        names.contains(&"keep.log"),
        "a negated rule keeps it: {names:?}"
    );
    assert!(
        names.contains(&"tracked.log"),
        "tracked is never ignored: {names:?}"
    );
    for gone in ["a.log", "build", "build/out.o", "sub/deep.log"] {
        assert!(!names.contains(&gone), "{gone} is ignored: {names:?}");
    }
    assert!(
        names.contains(&"sub"),
        "the directory itself is not ignored"
    );
}

#[test]
fn temporary_names_are_invisible_but_collected() {
    let t = tempfile::tempdir().unwrap();
    let r = t.path();
    write(&r.join("a.txt"), "x");
    write(&r.join(".replicant-tmp-a.txt"), "half");
    write(&r.join("d/.replicant-tmp-z"), "half");
    write(
        &r.join("target/.replicant-tmp-q"),
        "excluded dir: never walked",
    );
    let ex = Excludes::for_root(&["target".into()]).unwrap();
    let m = Manifest::walk(r, &ex, false).unwrap();
    assert_eq!(rels(&m), ["a.txt", "d"]);
    assert_eq!(m.temps, [".replicant-tmp-a.txt", "d/.replicant-tmp-z"]);
}

#[test]
fn names_differing_only_by_case_are_listed_as_the_filesystem_holds_them() {
    let t = tempfile::tempdir().unwrap();
    let r = t.path();
    write(&r.join("Readme"), "upper");
    write(&r.join("readme"), "lower");
    let folds = fs::read_dir(r).unwrap().count() == 1;
    let m = Manifest::walk(r, &none(), false).unwrap();
    match folds {
        true => assert_eq!(m.entries.len(), 1, "{m:?}"),
        false => assert_eq!(rels(&m), ["Readme", "readme"]),
    }
    // Guardrail 4 is the case walk's, fed the same names.
    if !folds {
        let pairs = crate::repos::case_collisions(rels(&m));
        assert_eq!(pairs, [("Readme".to_string(), "readme".to_string())]);
    }
}

#[test]
fn roots_missing_single_files_and_symlinked_bases() {
    let t = tempfile::tempdir().unwrap();
    let missing = Manifest::walk(&t.path().join("nope"), &none(), false).unwrap();
    assert_eq!(missing, Manifest::default());

    let file = t.path().join("settings.json");
    write(&file, "{}");
    let m = Manifest::walk(&file, &none(), true).unwrap();
    assert_eq!(rels(&m), [""]);
    assert_eq!((m.entries[0].kind, m.entries[0].size), (EntryKind::File, 2));

    let real = t.path().join("vol/work2");
    write(&real.join("x"), "x");
    let base = t.path().join("work2");
    std::os::unix::fs::symlink(&real, &base).unwrap();
    assert_eq!(rels(&Manifest::walk(&base, &none(), false).unwrap()), ["x"]);

    let dangling = t.path().join("gone");
    std::os::unix::fs::symlink(t.path().join("unmounted"), &dangling).unwrap();
    assert_eq!(
        Manifest::walk(&dangling, &none(), false).unwrap(),
        Manifest::default()
    );

    // A single-file unit that is a link is the link.
    let link = t.path().join("cfg");
    std::os::unix::fs::symlink("settings.json", &link).unwrap();
    let m = Manifest::walk(&link, &none(), true).unwrap();
    assert_eq!(m.entries[0].kind, EntryKind::Symlink);
}

#[test]
fn a_manifest_round_trips_through_its_short_wire_form() {
    let t = tempfile::tempdir().unwrap();
    write(&t.path().join("a/b"), "bytes");
    std::os::unix::fs::symlink("a/b", t.path().join("l")).unwrap();
    write(&t.path().join(".replicant-tmp-x"), "");
    let m = Manifest::walk(t.path(), &none(), false).unwrap();
    let json = serde_json::to_string(&m).unwrap();
    assert!(json.contains("\"p\":\"a/b\""), "{json}");
    assert_eq!(serde_json::from_str::<Manifest>(&json).unwrap(), m);
}

#[test]
fn a_dead_transfers_temp_file_is_not_state() {
    let t = tempfile::tempdir().unwrap();
    let loose = t.path().join("loose");
    write(&loose.join("a.txt"), "x");
    let r = testutil::repo(&t.path().join("r"));
    let ex = Excludes::for_root(&[]).unwrap();
    let hash = |p: &Path| crate::state::loose(&crate::repos::discover(p, &ex).unwrap().loose);
    let (before_loose, before_repo) = (hash(&loose), crate::state::repo(&r, &ex).unwrap());
    write(&loose.join(".replicant-tmp-a.txt"), "half");
    write(&r.join(".replicant-tmp-README"), "half");
    assert_eq!(hash(&loose), before_loose);
    let after = crate::state::repo(&r, &ex).unwrap();
    assert_eq!(after.state, before_repo.state);
    assert!(after.dirty.is_empty(), "{:?}", after.dirty);
}
