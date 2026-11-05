//! `sync` end to end: a home and a leaf in one tempdir (`Bed`), home
//! in-process, data connections over socket pairs. What is written is
//! exactly what the dry run itemized, baselines and markers land where PLAN.md
//! says, and every failure path leaves what it promises.

use super::*;
use crate::baseline::Baseline;
use crate::daemon::home::{Home, leaf_baselines};
pub(crate) use crate::decide::Decision;
use crate::marker::Marker;
pub(crate) use crate::plan::PlannedUnit;
use crate::testutil::{self, Bed, InProcess};
use std::collections::{BTreeMap, BTreeSet};
pub(crate) use std::path::Path;

#[path = "apply_abort_tests.rs"]
mod abort_tests;
#[path = "apply_block_tests.rs"]
mod block;
#[path = "apply_digest_tests.rs"]
mod digest_tests;
#[path = "apply_fail_tests.rs"]
pub(crate) mod fail;
#[path = "apply_hangup_tests.rs"]
mod hangup_tests;
#[path = "apply_marker_tests.rs"]
pub(crate) mod marker_tests;
#[path = "apply_more_tests.rs"]
mod more;
#[path = "apply_plan_compare_tests.rs"]
mod plan_compare_tests;
#[path = "apply_progress_tests.rs"]
pub(crate) mod progress_tests;
#[path = "apply_resume_tests.rs"]
mod resume_tests;

pub(crate) fn ctx<'a>(
    bed: &'a Bed,
    cfg: &'a crate::config::LeafConfig,
    state: &'a Path,
) -> LeafCtx<'a> {
    LeafCtx {
        me: "leaf",
        home_dir: &bed.leaf_hd,
        cfg,
        state_dir: state,
        plane: Default::default(),
        accept_rollback: &[],
    }
}

/// `sync` as `leaf`.
pub(crate) fn sync_with(bed: &Bed, home: &Home, opts: &SyncOpts) -> Result<SyncReport> {
    let link = InProcess {
        home,
        host: "leaf".into(),
        home_dir: bed.leaf_hd.clone(),
    };
    let cfg = bed.leaf_cfg();
    let dirs = bed.leaf_dirs.state();
    sync(&link, ctx(bed, &cfg, &dirs), opts)
}

/// How long an injected-failure sync may take before the binary is aborted
/// (review A36, A43): every such test ends in seconds.
pub(crate) const SYNC_LIMIT: std::time::Duration = std::time::Duration::from_secs(120);

/// `sync` as `leaf`, with `inj`'s failure injected into its unit.
pub(crate) fn sync_inject(
    bed: &Bed,
    home: &Home,
    opts: &SyncOpts,
    inj: &crate::plan::tests::inject::Inject,
) -> Result<SyncReport> {
    let link = inj.link(bed, home);
    let cfg = bed.leaf_cfg();
    let dirs = bed.leaf_dirs.state();
    let ctx = ctx(bed, &cfg, &dirs);
    // A deletion carried out here (home has nothing to send) is local, with
    // no connection to cut: it fails as a deletion does, on a directory it
    // may not write, put back as it was afterwards.
    use std::os::unix::fs::PermissionsExt;
    let unit_dir = bed.leaf_hd.join(&inj.unit);
    let local = inj.deleting && unit_dir.is_dir();
    let mode = |m| std::fs::set_permissions(&unit_dir, std::fs::Permissions::from_mode(m)).unwrap();
    if local {
        mode(0o555);
    }
    let r = {
        let _dog = crate::testutil::watchdog(
            SYNC_LIMIT,
            format!("sync_inject({:?}, {:?})", inj.unit, inj.how),
        );
        sync(&link, ctx, opts)
    };
    if local {
        mode(0o755);
    }
    r
}

pub(crate) fn yes(bed: &Bed, home: &Home) -> SyncReport {
    sync_with(bed, home, &SyncOpts::default()).unwrap()
}

pub(crate) fn plan_of(bed: &Bed, home: &Home, dry: bool) -> Plan {
    let link = InProcess {
        home,
        host: "leaf".into(),
        home_dir: bed.leaf_hd.clone(),
    };
    let cfg = bed.leaf_cfg();
    let dirs = bed.leaf_dirs.state();
    crate::plan::plan(&link, ctx(bed, &cfg, &dirs), dry, false).unwrap()
}

pub(crate) fn unit<'p>(p: &'p Plan, rel: &str, kind: UnitKind) -> &'p PlannedUnit {
    p.units
        .iter()
        .find(|u| u.rel == rel && u.kind == kind)
        .unwrap_or_else(|| panic!("no unit {rel}: {:#?}", p.units))
}

/// The tree under `base`: two clean repos, one dirty, loose files, and a
/// build directory the root excludes.
pub(crate) fn tree(base: &Path) {
    testutil::repo(&base.join("alpha"));
    let beta = testutil::repo(&base.join("beta"));
    testutil::write(&beta.join("README"), "changed, uncommitted\n");
    testutil::write(&beta.join("new.txt"), "untracked\n");
    testutil::repo(&base.join("nested/gamma"));
    testutil::write(&base.join("notes.txt"), "loose\n");
    testutil::write(&base.join("data/blob.bin"), "0123456789");
    testutil::write(&base.join("target/junk"), "build output");
}

pub(crate) type Print = BTreeMap<String, (Vec<u8>, u32, std::time::SystemTime)>;

/// Paths whose file content, mode or mtime changed, or that appeared or
/// disappeared (files, and directories by existence only: a directory's
/// mtime moves whenever its contents do).
pub(crate) fn changed(before: &Print, after: &Print) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for k in before.keys().chain(after.keys()) {
        let (b, a) = (before.get(k), after.get(k));
        let differs = match (b, a) {
            (Some(_), Some(_)) if k.ends_with('/') => false,
            (Some(x), Some(y)) => x != y,
            _ => true,
        };
        if differs {
            out.insert(k.trim_end_matches('/').to_string());
        }
    }
    out
}

/// The target changed in exactly the paths the transfer itemized. A unit's own
/// directory is the one exception: it is not an entry of its manifest, so a
/// unit root may appear (or vanish) without an itemized line of its own.
pub(crate) fn wrote_exactly(before: &Print, after: &Print, r: &SyncReport) {
    let roots: BTreeSet<String> = r.applied.iter().map(|a| a.rel.clone()).collect();
    let got: BTreeSet<String> = changed(before, after).difference(&roots).cloned().collect();
    assert_eq!(got, itemized(r));
}

/// What the report says the transfer changed, as paths under the tree root
/// (`work/alpha/README`), attribute changes of directories left out.
pub(crate) fn itemized(r: &SyncReport) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for a in &r.applied {
        for c in &a.changes {
            let (kind, path) = c.split_at(2);
            let is_dir = path.ends_with('/');
            if kind == "a " && is_dir {
                continue;
            }
            let path = path.trim_end_matches('/');
            out.insert(if path.is_empty() {
                a.rel.clone()
            } else {
                format!("{}/{path}", a.rel)
            });
        }
    }
    out
}

fn markers(state: &Path) -> Vec<Marker> {
    crate::marker::store(state).all().unwrap()
}

#[test]
fn a_pull_writes_exactly_what_its_dry_run_itemized_and_records_both_baselines() {
    let bed = Bed::enrolled();
    tree(&bed.home_hd.join("work"));
    let home = bed.home();
    let home_before = testutil::fingerprint(&bed.home_hd);
    let leaf_before = testutil::fingerprint(&bed.leaf_hd);
    let r = yes(&bed, &home);
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    assert_eq!(
        testutil::fingerprint(&bed.home_hd),
        home_before,
        "the source is untouched"
    );
    let leaf_after = testutil::fingerprint(&bed.leaf_hd);
    wrote_exactly(&leaf_before, &leaf_after, &r);
    let actions: Vec<(&str, &str)> = r
        .applied
        .iter()
        .map(|a| (a.rel.as_str(), a.action.as_str()))
        .collect();
    assert_eq!(
        actions,
        [
            ("work", "pull"),
            ("work/alpha", "pull"),
            ("work/beta", "pull"),
            ("work/nested/gamma", "pull")
        ]
    );
    assert!(!bed.leaf_hd.join("work/target").exists(), "excluded");
    assert!(markers(&bed.leaf_dirs.state()).is_empty());
    assert!(markers(&bed.home_dirs.state()).is_empty());

    // Both sides recorded the same baselines, and the next look agrees.
    let leaf_b: Vec<Baseline> = crate::baseline::store(&bed.leaf_dirs.state())
        .all()
        .unwrap();
    let home_b: Vec<Baseline> = leaf_baselines(&bed.home_dirs.state(), "leaf")
        .all()
        .unwrap();
    assert_eq!(leaf_b.len(), 4);
    assert_eq!(
        leaf_b
            .iter()
            .map(|b| (&b.rel, &b.leaf, &b.home))
            .collect::<Vec<_>>(),
        home_b
            .iter()
            .map(|b| (&b.rel, &b.leaf, &b.home))
            .collect::<Vec<_>>()
    );
    let after = plan_of(&bed, &home, false);
    for u in &after.units {
        assert_eq!(
            u.decision,
            Decision::Nothing {
                why: "unchanged".into()
            },
            "{}",
            u.rel
        );
    }
    let log = home.log(&Default::default()).unwrap();
    let synced: Vec<&str> = log
        .iter()
        .filter(|l| {
            l.event == "sync" && l.detail.ends_with(" ok") && !l.detail.contains(" units, ")
        })
        .map(|l| l.path.as_str())
        .collect();
    assert_eq!(
        synced,
        ["work", "work/alpha", "work/beta", "work/nested/gamma"]
    );
    assert!(
        log.iter().any(|l| l.event == "sync"
            && l.detail.starts_with("4 units, ")
            && l.detail.ends_with("; ok")),
        "{log:#?}"
    );
    assert!(home.queue.view().is_empty(), "released");
}

#[test]
fn a_push_writes_exactly_what_its_dry_run_itemized_on_home() {
    let bed = Bed::enrolled();
    tree(&bed.leaf_hd.join("work"));
    let home = bed.home();
    let leaf_before = testutil::fingerprint(&bed.leaf_hd);
    let home_before = testutil::fingerprint(&bed.home_hd);
    let r = yes(&bed, &home);
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    assert_eq!(testutil::fingerprint(&bed.leaf_hd), leaf_before);
    let home_after = testutil::fingerprint(&bed.home_hd);
    wrote_exactly(&home_before, &home_after, &r);
    assert!(r.applied.iter().all(|a| a.action == "push"));
    assert!(markers(&bed.home_dirs.state()).is_empty(), "cleared");
    for u in &plan_of(&bed, &home, false).units {
        assert_eq!(
            u.decision,
            Decision::Nothing {
                why: "unchanged".into()
            },
            "{}",
            u.rel
        );
    }

    // A change on the leaf is now a push of that change alone.
    testutil::write(&bed.leaf_hd.join("work/notes.txt"), "edited on the leaf\n");
    let before = testutil::fingerprint(&bed.home_hd);
    let r = yes(&bed, &home);
    assert_eq!(r.outcome, Outcome::Done);
    assert_eq!(r.applied.len(), 1, "{:#?}", r.applied);
    assert_eq!(r.applied[0].rel, "work");
    let got = changed(&before, &testutil::fingerprint(&bed.home_hd));
    assert_eq!(got, BTreeSet::from(["work/notes.txt".to_string()]));
    assert_eq!(got, itemized(&r));
}
