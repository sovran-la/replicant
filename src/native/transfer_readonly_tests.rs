//! A directory read-only on the target that the source agrees about (the
//! re-review's m10): no difference in itself, so not in `mkdir`, and whatever
//! changed inside it used to fail with `Permission denied`, every run. It is
//! opened for the transfer and has its mode back at the end, either way.

use super::*;
use crate::native::tests_fixture::{Fx, manifest, run};
use crate::testutil::write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

const RO: u32 = 0o555;
const BOTH: [Direction; 2] = [Direction::Push, Direction::Pull];

fn chmod(p: &Path, mode: u32) {
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn mode(p: &Path) -> u32 {
    std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777
}

/// Owner-writable again, all of it: a tempdir cleans up only what it can.
fn open_all(p: &Path) {
    if std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()) {
        chmod(p, 0o755);
        for e in std::fs::read_dir(p).unwrap() {
            open_all(&e.unwrap().path());
        }
    }
}

/// `make` builds (source, target); the transfer runs, either way, and must
/// finish and leave nothing to do; `check` looks at the target before the
/// tempdir goes. What it returns is the dry run's change list, as it reads.
fn carried(
    direction: Direction,
    make: impl FnOnce(&Path, &Path),
    check: impl FnOnce(&Path),
) -> Vec<String> {
    let fx = Fx::new();
    let (source, target) = match direction {
        Direction::Push => (fx.leaf(), fx.home_work()),
        Direction::Pull => (fx.home_work(), fx.leaf()),
    };
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    make(&source, &target);
    let lease = fx.lease();
    if direction == Direction::Push {
        fx.begin_push(lease);
    }
    let t = fx.transfer(direction);
    let (d, ran, _) = run(
        &fx.link(),
        &t,
        lease,
        4,
        &Abort::default(),
        Duration::from_secs(20),
    );
    let again = {
        let progress = |_: Counts| {};
        let abort = Abort::default();
        let link = fx.link();
        let env = Env {
            link: &link,
            me: "leaf",
            lease,
            abort: &abort,
            streams: 1,
            ack_timeout: Duration::from_secs(20),
            progress: &progress,
        };
        t.dry_run(&env).unwrap()
    };
    fx.release(lease);
    let (ok, why) = (ran.ok(), ran.why());
    if ok {
        check(&target);
    }
    // The whole bed: a rollback copy of `ro/` lands read-only under home's
    // backup dir too, and a tempdir that cannot be removed is left behind on
    // every run (review A39).
    open_all(fx.bed.tmp.path());
    assert!(ok, "{direction:?}: {why}");
    assert!(again.changes.only_kept_dirs(), "{direction:?}: {again:?}");
    d.itemized.changes.iter().map(|c| c.path.clone()).collect()
}

/// `ro/`, read-only on both sides, holding `f.txt` on both.
fn both_read_only(source: &Path, target: &Path) {
    for (side, text) in [(source, "new contents"), (target, "old")] {
        write(&side.join("ro/f.txt"), text);
        chmod(&side.join("ro"), RO);
    }
}

/// The review's n1.
#[test]
fn a_changed_file_in_a_directory_read_only_on_both_sides_arrives() {
    for direction in BOTH {
        let listed = carried(direction, both_read_only, |target| {
            assert_eq!(
                std::fs::read_to_string(target.join("ro/f.txt")).unwrap(),
                "new contents"
            );
            assert_eq!(mode(&target.join("ro")), RO, "its mode is back");
        });
        assert_eq!(listed, ["ro/f.txt"], "and the plan says nothing about ro/");
    }
}

#[test]
fn many_files_over_many_connections_all_fit_in_before_its_mode_is_back() {
    for direction in BOTH {
        let make = |source: &Path, target: &Path| {
            for i in 0..40 {
                write(&source.join(format!("ro/{i}.txt")), &format!("new {i}"));
                write(&target.join(format!("ro/{i}.txt")), "old");
            }
            chmod(&source.join("ro"), RO);
            chmod(&target.join("ro"), RO);
        };
        carried(direction, make, |target| {
            for i in 0..40 {
                assert_eq!(
                    std::fs::read_to_string(target.join(format!("ro/{i}.txt"))).unwrap(),
                    format!("new {i}")
                );
            }
            assert_eq!(mode(&target.join("ro")), RO);
        });
    }
}

#[test]
fn a_new_file_a_new_link_and_a_new_directory_land_in_one() {
    for direction in BOTH {
        let make = |source: &Path, target: &Path| {
            write(&source.join("ro/f.txt"), "same");
            write(&source.join("ro/new.txt"), "new");
            write(&source.join("ro/sub/in.txt"), "in");
            symlink("f.txt", source.join("ro/l")).unwrap();
            std::fs::create_dir_all(target.join("ro")).unwrap();
            chmod(&source.join("ro"), RO);
            chmod(&target.join("ro"), RO);
        };
        carried(direction, make, |target| {
            assert_eq!(
                std::fs::read_to_string(target.join("ro/sub/in.txt")).unwrap(),
                "in"
            );
            assert_eq!(
                std::fs::read_link(target.join("ro/l")).unwrap(),
                Path::new("f.txt")
            );
            assert!(target.join("ro/new.txt").exists());
            assert_eq!(mode(&target.join("ro")), RO);
        });
    }
}

#[test]
fn a_deletion_inside_one_is_carried_out() {
    for direction in BOTH {
        let make = |source: &Path, target: &Path| {
            write(&source.join("ro/keep.txt"), "x");
            write(&target.join("ro/keep.txt"), "x");
            write(&target.join("ro/gone.txt"), "x");
            chmod(&source.join("ro"), RO);
            chmod(&target.join("ro"), RO);
        };
        carried(direction, make, |target| {
            assert!(!target.join("ro/gone.txt").exists());
            assert!(target.join("ro/keep.txt").exists());
            assert_eq!(mode(&target.join("ro")), RO);
        });
    }
}

#[test]
fn a_kind_change_inside_one_is_carried_out() {
    for direction in BOTH {
        let make = |source: &Path, target: &Path| {
            write(&source.join("ro/was-file/in.txt"), "x");
            write(&source.join("ro/was-dir"), "x");
            write(&target.join("ro/was-file"), "x");
            write(&target.join("ro/was-dir/in.txt"), "x");
            chmod(&source.join("ro"), RO);
            chmod(&target.join("ro"), RO);
        };
        carried(direction, make, |target| {
            assert!(target.join("ro/was-file/in.txt").exists());
            assert!(target.join("ro/was-dir").is_file());
            assert_eq!(mode(&target.join("ro")), RO);
        });
    }
}

#[test]
fn a_read_only_directory_only_the_target_has_goes_with_its_contents() {
    for direction in BOTH {
        let make = |source: &Path, target: &Path| {
            write(&source.join("keep.txt"), "x");
            write(&target.join("keep.txt"), "x");
            write(&target.join("gone/deep/f.txt"), "x");
            chmod(&target.join("gone/deep"), RO);
            chmod(&target.join("gone"), RO);
        };
        carried(direction, make, |target| {
            assert!(!target.join("gone").exists());
        });
    }
}

/// `target/` is excluded here: nothing may delete it, so `kept/` stays, and
/// stays what it was.
#[test]
fn a_read_only_directory_kept_for_what_the_unit_excludes_has_its_mode_back() {
    for direction in BOTH {
        let make = |source: &Path, target: &Path| {
            write(&source.join("keep.txt"), "x");
            write(&target.join("keep.txt"), "x");
            write(&target.join("kept/gone.txt"), "x");
            write(&target.join("kept/target/junk"), "x");
            chmod(&target.join("kept"), RO);
        };
        carried(direction, make, |target| {
            assert!(!target.join("kept/gone.txt").exists());
            assert!(target.join("kept/target/junk").exists());
            assert_eq!(mode(&target.join("kept")), RO);
        });
    }
}

#[test]
fn a_read_only_directory_the_source_made_a_file_is_replaced() {
    for direction in BOTH {
        let make = |source: &Path, target: &Path| {
            write(&source.join("d"), "a file now");
            write(&target.join("d/in.txt"), "x");
            chmod(&target.join("d"), RO);
        };
        carried(direction, make, |target| {
            assert_eq!(
                std::fs::read_to_string(target.join("d")).unwrap(),
                "a file now"
            );
        });
    }
}

/// Rollback must get back into it too, and leave it as it was.
#[test]
fn a_push_into_one_rolls_back_exactly() {
    let fx = Fx::new();
    let (source, target) = (fx.leaf(), fx.home_work());
    both_read_only(&source, &target);
    let before = manifest(&target);
    let lease = fx.lease();
    let id = fx.begin_push(lease);
    let t = fx.transfer(Direction::Push);
    let (_, ran, _) = run(
        &fx.link(),
        &t,
        lease,
        2,
        &Abort::default(),
        Duration::from_secs(20),
    );
    assert!(ran.ok(), "{ran:?}");
    fx.link()
        .call(HomeRequest::Settle {
            lease,
            rel: "work".into(),
            direction: Some(Direction::Push),
            detail: "push ok".into(),
            changes: vec!["~ ro/f.txt".into()],
        })
        .unwrap();
    fx.release(lease);
    let r = fx.home.rollback(&id).unwrap();
    assert!(r.unbacked.is_empty() && r.unreported.is_empty(), "{r:?}");
    let after = manifest(&target);
    let text = std::fs::read_to_string(target.join("ro/f.txt"));
    open_all(&source);
    open_all(&target);
    assert_eq!(after, before, "sizes, mtimes, modes, ro/ included");
    assert_eq!(text.unwrap(), "old");
}

/// Known limits: the unit's own root is in no manifest, so nothing has a mode
/// for it. It fails, naming the path, and loses nothing.
#[test]
fn a_read_only_unit_root_still_fails_and_says_where() {
    let fx = Fx::new();
    let (source, target) = (fx.home_work(), fx.leaf());
    write(&source.join("f.txt"), "new contents");
    write(&target.join("f.txt"), "old");
    chmod(&target, RO);
    let lease = fx.lease();
    let t = fx.transfer(Direction::Pull);
    let abort = Abort::default();
    let (_, ran, _) = run(&fx.link(), &t, lease, 1, &abort, Duration::from_secs(20));
    fx.release(lease);
    chmod(&target, 0o755);
    assert!(ran.why().contains("f.txt"), "{ran:?}");
    assert!(!ran.ok());
    assert_eq!(
        std::fs::read_to_string(target.join("f.txt")).unwrap(),
        "old"
    );
}
