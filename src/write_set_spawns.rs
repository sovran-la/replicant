//! The write set's list, continued: every process replicant starts, and
//! every way into rsync, the only writer of synced paths (review A4). A
//! new call of any of these is a new rsync run, so it is counted like a
//! write.

/// (file, pattern, count, why).
pub(super) const SPAWNS: &[(&str, &str, usize, &str)] = &[
    // Processes.
    (
        "rsync/child.rs",
        "Command::new(",
        1,
        "rsync: the only writer of synced paths",
    ),
    (
        "rsync/client.rs",
        "Command::new(",
        1,
        "rsync --version: reads nothing",
    ),
    (
        "git.rs",
        "Command::new(",
        1,
        "git, read-only (no optional locks)",
    ),
    // rsync's entry points: every real or dry run goes through child::run.
    (
        "rsync.rs",
        "child::run(",
        1,
        "Runner::run: the dry run (`--dry-run` always) and Transfer::apply",
    ),
    (
        "rsync/apply.rs",
        "child::run(",
        3,
        "Transfer::apply (the real transfer), delete_listed, copy_listed",
    ),
    (
        "rsync/apply.rs",
        "delete_listed(",
        2,
        "its definition, and a single-file unit's deletion through it",
    ),
    ("rsync/apply.rs", "copy_listed(", 1, "its definition"),
    // `::run(` (re-review R3) counts every `X::run(`: the three `child::run(`
    // above again, and calls that never reach rsync.
    ("rsync.rs", "::run(", 1, "the `child::run(` in Runner::run"),
    (
        "rsync.rs",
        ".run(",
        1,
        "Transfer::dry_run through Runner::run (`--dry-run` asserted)",
    ),
    (
        "rsync/apply.rs",
        "::run(",
        3,
        "the three `child::run(` counted above",
    ),
    (
        "plan/mod.rs",
        "::run(",
        2,
        "Discovered::run (a read-only walk) and diff::run (the dry runs)",
    ),
    (
        "plan/unserved.rs",
        "::run(",
        1,
        "Discovered::run: a read-only walk",
    ),
    (
        "daemon/home_serve.rs",
        "::run(",
        1,
        "Discovered::run: a read-only walk",
    ),
    (
        "cli/mod.rs",
        "::run(",
        2,
        "approve::run and sync::run: CLI subcommands, which write through the daemons",
    ),
    (
        "plan/apply_unit.rs",
        ".apply(",
        1,
        "the unit's real transfer (Ops::apply), after its marker: synced paths",
    ),
    (
        "plan/xfer.rs",
        ".apply(",
        2,
        "Ops::apply: rsync's Transfer::apply, or the native transfer's apply",
    ),
    (
        "daemon/home_backup.rs",
        "copy_listed(",
        1,
        "native::local: a BeginPush list copied into backup/<push>/run-<n>/",
    ),
    (
        "daemon/home_rollback.rs",
        "copy_listed(",
        2,
        "native::local: rollback keeps what the push wrote, then restores from the backup",
    ),
    (
        "daemon/home_rollback.rs",
        "delete_listed(",
        1,
        "native::local: rollback deletes what the push created",
    ),
];
