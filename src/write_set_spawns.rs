//! The write set's list, continued: every process replicant starts, and
//! every way into a transfer or a list operation, the writers of synced
//! paths (review A4). A new call of any of these is a new write of synced
//! paths, so it is counted like one.

/// (file, pattern, count, why).
pub(super) const SPAWNS: &[(&str, &str, usize, &str)] = &[
    // Processes.
    (
        "git.rs",
        "Command::new(",
        1,
        "git, read-only (no optional locks); `hash-object` reads files and writes nothing",
    ),
    // A unit's transfer: the receiver writes synced paths (native/).
    (
        "plan/apply_unit.rs",
        ".apply(",
        1,
        "the unit's transfer (Ops::apply), after its marker: synced paths",
    ),
    (
        "plan/xfer.rs",
        ".apply(",
        1,
        "Ops::apply: the native transfer's apply",
    ),
    // Home's list operations on a unit (native::local, through the receiver).
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
