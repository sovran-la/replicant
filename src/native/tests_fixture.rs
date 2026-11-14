//! A home in-process and a leaf in one tempdir, for transfers: the unit is
//! the loose `work` (root exclude `target`), data connections are socket
//! pairs, and [`Faulty`] can cut or stall one of them.

use super::manifest::Manifest;
use super::transfer::{Counts, Diffed, Env, Ran, Transfer};
use crate::abort::Abort;
use crate::daemon::home::Home;
use crate::daemon::wire::{HomeReply, HomeRequest};
use crate::exclude::Excludes;
use crate::marker::Direction;
use crate::plan::HomeLink;
use crate::testutil::{Bed, InProcess, write};
use crate::units::{SubSpec, UnitKind};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

pub struct Fx {
    pub bed: Bed,
    pub home: Home,
}

pub fn excludes() -> Excludes {
    let spec = SubSpec {
        rel: "work".into(),
        exclude: vec!["target".into()],
        except: vec![],
        root: "work".into(),
    };
    crate::units::excludes(&spec, "work", UnitKind::Loose, &[], &[]).unwrap()
}

pub fn manifest(p: &Path) -> Manifest {
    Manifest::walk(p, &excludes(), false).unwrap()
}

/// Small files, a few large ones (several chunks each), links, nested and
/// empty directories, odd modes and nanosecond mtimes.
pub fn tree(base: &Path, big: usize) {
    use std::os::unix::fs::PermissionsExt;
    for i in 0..120 {
        write(
            &base.join(format!("small/{:03}/f{i}.txt", i % 7)),
            &format!("small {i}\n"),
        );
    }
    for i in 0..big {
        let bytes: Vec<u8> = (0..(1 << 20) * 2 + 12_345 * (i + 1))
            .map(|b| (b * 7 + i) as u8)
            .collect();
        std::fs::create_dir_all(base.join("big")).unwrap();
        std::fs::write(base.join(format!("big/blob{i}.bin")), bytes).unwrap();
    }
    write(&base.join("nested/a/b/c/deep.txt"), "deep");
    std::fs::create_dir_all(base.join("empty/dir")).unwrap();
    write(&base.join("exec.sh"), "#!/bin/sh\n");
    std::fs::set_permissions(base.join("exec.sh"), std::fs::Permissions::from_mode(0o751)).unwrap();
    std::fs::set_permissions(
        base.join("nested/a"),
        std::fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    std::os::unix::fs::symlink("nested/a/b", base.join("link-dir")).unwrap();
    std::os::unix::fs::symlink("/nonexistent/target", base.join("dangling")).unwrap();
    write(&base.join("target/junk"), "excluded: never moves");
    super::manifest::tests::set_mtime(&base.join("exec.sh"), 1_650_000_000, 123_456_789);
}

impl Fx {
    pub fn new() -> Self {
        let bed = Bed::enrolled();
        let home = bed.home();
        Self { bed, home }
    }

    pub fn link(&self) -> InProcess<'_> {
        InProcess {
            home: &self.home,
            host: "leaf".into(),
            home_dir: self.bed.leaf_hd.clone(),
        }
    }

    pub fn leaf(&self) -> PathBuf {
        self.bed.leaf_hd.join("work")
    }
    pub fn home_work(&self) -> PathBuf {
        self.bed.home_hd.join("work")
    }

    /// A sync's queue entry for `leaf`.
    pub fn lease(&self) -> u64 {
        match self
            .link()
            .call(HomeRequest::Acquire {
                no_wait: true,
                apply: true,
            })
            .unwrap()
        {
            HomeReply::Lease { lease, .. } => lease,
            other => panic!("{other:?}"),
        }
    }

    pub fn release(&self, lease: u64) {
        let _ = self.link().call(HomeRequest::Release { lease });
    }

    /// Home's marker and push run, as `BeginPush` makes them before a push.
    pub fn begin_push(&self, lease: u64) -> String {
        match self
            .link()
            .call(HomeRequest::BeginPush {
                lease,
                rel: "work".into(),
                source_state: crate::state::UnitState::Empty,
                is_file: false,
            })
            .unwrap()
        {
            HomeReply::Begun { push } => push,
            other => panic!("{other:?}"),
        }
    }

    pub fn transfer(&self, direction: Direction) -> Transfer {
        Transfer {
            direction,
            rel: "work".into(),
            kind: UnitKind::Loose,
            leaf_path: self.leaf(),
            is_file: false,
            source_absent: false,
            excludes: excludes(),
            union: vec![],
            worktrees: vec![],
            ignored: vec![],
            backup: None,
        }
    }
}

/// Run a dry run and its apply; the progress reported, in order.
pub fn run(
    link: &dyn HomeLink,
    t: &Transfer,
    lease: u64,
    streams: u32,
    abort: &Abort,
    ack: Duration,
) -> (Diffed, Ran, Vec<Counts>) {
    let seen: Mutex<Vec<Counts>> = Mutex::default();
    let progress = |c: Counts| seen.lock().unwrap().push(c);
    let env = Env {
        link,
        me: "leaf",
        lease,
        abort,
        streams,
        ack_timeout: ack,
        progress: &progress,
    };
    let d = t.dry_run(&env).unwrap();
    let ran = t.apply(&env, &d);
    (d, ran, seen.into_inner().unwrap())
}

/// Run `f` on a thread of its own and return what it returns, failing the
/// test if that takes longer than `limit`: for what must not hang. (A hung
/// `f` stays behind, blocked, until the test process exits.)
pub fn bounded<T: Send + 'static>(limit: Duration, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(limit)
        .unwrap_or_else(|e| panic!("still not done after {limit:?}: {e}"))
}

pub use super::tests_faulty::{Fault, Faulty};
