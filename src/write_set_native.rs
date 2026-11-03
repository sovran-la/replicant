//! The write set's list, continued: the native data plane's writes. Every
//! write of a synced path is the receiver's (`native/receive*.rs`), home's
//! backup by rename (`native/backup.rs`), or a call into them.

/// (file, pattern, count, why).
pub(super) const NATIVE: &[(&str, &str, usize, &str)] = &[
    // Synced paths: the native receiver (home on a push, the leaf on a
    // pull), and home's backup of what a push replaces.
    (
        "native/receive.rs",
        "create_dir_all(",
        1,
        "a missing unit root (or a single-file unit's directory): synced paths",
    ),
    (
        "native/receive.rs",
        "create_dir(",
        1,
        "Receiver::mkdir, a directory of the source: synced paths",
    ),
    (
        "native/receive.rs",
        "set_permissions(",
        2,
        "a new directory owner-writable, then dir_modes' final mode: synced paths",
    ),
    (
        "native/receive.rs",
        "remove_dir(",
        2,
        "replace's emptied directory, and a deleted directory: synced paths",
    ),
    (
        "native/receive.rs",
        "remove_file(",
        1,
        "clean: a dead transfer's temporary name only: synced unit",
    ),
    (
        "native/receive.rs",
        ".delete(",
        1,
        "replace deleting what lies under a directory first: synced paths",
    ),
    (
        "native/receive_file.rs",
        "OpenOptions::new(",
        1,
        "a file's temporary (create_new, 0600): synced unit",
    ),
    (
        "native/receive_file.rs",
        "set_permissions(",
        1,
        "the temporary gets the source's mode: synced unit",
    ),
    (
        "native/receive_file.rs",
        "set_times(",
        1,
        "the temporary gets the source's mtime: synced unit",
    ),
    (
        "native/receive_file.rs",
        "set_modified(",
        1,
        "the FileTimes passed to that set_times",
    ),
    (
        "native/receive_file.rs",
        "fs::symlink(",
        1,
        "a link made under its temporary name: synced unit",
    ),
    (
        "native/receive_file.rs",
        "fs::rename(",
        1,
        "commit: the finished temporary over its target: synced paths",
    ),
    (
        "native/receive_file.rs",
        "remove_file(",
        4,
        "a stale or failed temporary, a dropped Incoming's: synced unit",
    ),
    (
        "native/backup.rs",
        "create_dir_all(",
        1,
        "a push run's backup dir: backup/",
    ),
    (
        "native/backup.rs",
        "fs::rename(",
        1,
        "a replaced or deleted path renamed into the backup: synced path to backup/",
    ),
    (
        "native/backup.rs",
        "fs::copy(",
        1,
        "the EXDEV fallback: a file copied into backup/",
    ),
    (
        "native/backup.rs",
        "fs::symlink(",
        1,
        "the EXDEV fallback: a link recreated in backup/",
    ),
    (
        "native/backup.rs",
        "create_dir(",
        1,
        "the EXDEV fallback: a directory in backup/",
    ),
    (
        "native/backup.rs",
        "set_permissions(",
        1,
        "the EXDEV fallback: that directory's mode",
    ),
    (
        "native/backup.rs",
        "set_times(",
        1,
        "the EXDEV fallback: the copied file's mtime",
    ),
    (
        "native/backup.rs",
        "set_modified(",
        1,
        "the FileTimes passed to that set_times",
    ),
    (
        "native/backup.rs",
        "remove_dir_all(",
        1,
        "remove_tree: the original after its EXDEV copy, or a run's second copy; a synced path",
    ),
    (
        "native/backup.rs",
        "remove_file(",
        1,
        "remove_tree, as above, for a file or link",
    ),
    (
        "native/serve.rs",
        ".delete(",
        1,
        "home receiving a push's deletions: synced paths",
    ),
    (
        "native/source.rs",
        "OpenOptions::new(",
        1,
        "the sender's open: read-only, O_NOFOLLOW; writes nothing",
    ),
    (
        "native/transfer_run.rs",
        ".delete(",
        1,
        "a pull's deletions on the leaf, after every file arrived: synced paths",
    ),
    (
        "native/local.rs",
        "create_dir_all(",
        1,
        "copy_listed's destination root (backup/, or the unit on rollback)",
    ),
    (
        "native/local.rs",
        "copy_listed(",
        1,
        "its definition: copies through the receiver",
    ),
    (
        "native/local.rs",
        "delete_listed(",
        1,
        "its definition: deletes through the receiver",
    ),
    (
        "native/local.rs",
        ".delete(",
        1,
        "delete_listed's call into the receiver: rollback, synced paths",
    ),
];
