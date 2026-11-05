//! The write set's list: each spawn and write outside the tests, by file,
//! with the part of the write set it is.

/// (file, pattern, count, why).
/// Processes, and every way into a transfer, are in `write_set_spawns.rs`;
/// the native receiver's writes of synced paths in `write_set_native.rs`.
pub(super) const ALLOWED: &[(&str, &str, usize, &str)] = &[
    // A scratch copy of a repo's index in the temp dir, for `git diff
    // HEAD` to refresh instead of the repo's own (guardrail 1, rev 18).
    // Never under a synced path.
    (
        "git.rs",
        "File::options(",
        1,
        "Scratch::copy_of: temp dir, created new",
    ),
    (
        "git.rs",
        "fs::copy(",
        1,
        "Scratch::copy_of: the index into the temp dir",
    ),
    (
        "git.rs",
        "remove_file(",
        1,
        "Scratch::drop: its own temp file",
    ),
    // Records under state/: baselines, markers, contacts.
    ("record.rs", "write_atomic(", 1, "Store::put: state dir"),
    ("record.rs", "remove_file(", 1, "Store::delete: state dir"),
    (
        "plan/apply_unit.rs",
        ".put(",
        2,
        "leaf marker and baselines: state dir",
    ),
    (
        "plan/apply_after.rs",
        ".put(",
        1,
        "an adopted baseline: state dir",
    ),
    (
        "plan/apply_after.rs",
        ".delete(",
        1,
        "a baseline of a unit gone on both sides: state dir",
    ),
    (
        "plan/apply_unit.rs",
        ".delete(",
        2,
        "leaf baseline and marker: state dir",
    ),
    (
        "daemon/home_apply.rs",
        ".put(",
        2,
        "home's baselines and push markers: state dir",
    ),
    (
        "daemon/home_apply.rs",
        ".delete(",
        2,
        "home's baselines and push markers: state dir",
    ),
    ("daemon/fleet.rs", ".put(", 2, "contacts: state dir"),
    // Config dir files.
    (
        "config/mod.rs",
        "write_atomic(",
        1,
        "the atomic writer itself",
    ),
    (
        "config/mod.rs",
        "create_dir_all(",
        1,
        "write_atomic's parent: config dir",
    ),
    (
        "config/mod.rs",
        "fs::write(",
        1,
        "write_atomic's temporary: config dir",
    ),
    (
        "config/mod.rs",
        "fs::rename(",
        1,
        "write_atomic's rename: config dir",
    ),
    ("config/edit.rs", "write_atomic(", 1, "fleet.toml"),
    ("config/leaf.rs", "write_atomic(", 1, "leaf.toml"),
    (
        "daemon/certs.rs",
        "create_dir_all(",
        1,
        "home's identity files: config dir",
    ),
    (
        "daemon/certs.rs",
        "OpenOptions::new(",
        1,
        "home's identity files: config dir",
    ),
    (
        "daemon/certs.rs",
        "set_permissions(",
        1,
        "home's identity files: config dir",
    ),
    (
        "daemon/certs.rs",
        "fs::rename(",
        1,
        "home's identity files: config dir",
    ),
    (
        "daemon/home_enroll.rs",
        "remove_file(",
        2,
        "issued certificates: state dir",
    ),
    (
        "daemon/home_enroll.rs",
        "create_dir_all(",
        1,
        "issued certificates: state dir",
    ),
    (
        "daemon/home_enroll.rs",
        "set_permissions(",
        1,
        "issued certificates: state dir",
    ),
    (
        "daemon/mod.rs",
        "create_dir_all(",
        1,
        "the config dir itself",
    ),
    (
        "daemon/mod.rs",
        "set_permissions(",
        2,
        "the config dir and its socket",
    ),
    (
        "daemon/mod.rs",
        "File::options(",
        1,
        "the daemon lock file: config dir",
    ),
    (
        "daemon/mod.rs",
        "remove_file(",
        2,
        "the daemon socket: config dir",
    ),
    (
        "daemon/journal.rs",
        "create_dir_all(",
        1,
        "the journal: config dir",
    ),
    (
        "daemon/journal.rs",
        "OpenOptions::new(",
        1,
        "the journal: config dir",
    ),
    (
        "daemon/journal.rs",
        "append_raw(",
        2,
        "the journal: config dir",
    ),
    (
        "daemon/leaf_link.rs",
        "append_raw(",
        1,
        "the leaf's copy of home's journal lines",
    ),
    // Home's push backups (backup/) and their manifests (state/pushes/).
    (
        "daemon/home_backup.rs",
        "create_dir_all(",
        1,
        "a push run's backup dir: backup/",
    ),
    (
        "daemon/home_backup.rs",
        ".put(",
        2,
        "push manifests: state dir",
    ),
    (
        "daemon/home_sweep.rs",
        "remove_dir_all(",
        1,
        "swept backups: backup/",
    ),
    (
        "daemon/home_sweep.rs",
        ".delete(",
        1,
        "a swept push's manifest: state dir",
    ),
    (
        "daemon/home_rolled_back.rs",
        ".put(",
        1,
        "a rollback settled, on its push's manifest: state dir",
    ),
    (
        "daemon/home_rollback.rs",
        "create_dir_all(",
        1,
        "a rollback's copy of what it undoes: backup/",
    ),
    (
        "daemon/home_rollback.rs",
        ".put(",
        1,
        "the rolled-back push's manifest: state dir",
    ),
    (
        "daemon/home_rollback.rs",
        ".delete(",
        1,
        "the rolled-back push's marker: state dir",
    ),
];
