//! A file that cannot be finished leaves no temporary behind.

use super::*;
use crate::native::manifest::{Entry, EntryKind};
use crate::testutil::write;

/// The review's nit on `finish`: it let go of the file ahead of the steps that
/// can fail, so a failed one left the temporary until the next run. `/dev/null`
/// is a descriptor nobody but root may chmod: the first step fails.
#[test]
fn a_file_that_cannot_be_finished_takes_its_temporary_with_it() {
    // SAFETY: no arguments, no failure.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let t = tempfile::tempdir().unwrap();
    write(&t.path().join("a"), "old");
    let rx = Receiver::open(t.path(), false, None, None).unwrap();
    let e = Entry {
        rel: "a".into(),
        kind: EntryKind::File,
        size: 0,
        mtime_ns: 1,
        mode: 0o644,
        link: None,
    };
    let mut inc = rx.begin(&e).unwrap();
    let temp = t.path().join(".replicant-tmp-a");
    assert!(temp.exists());
    inc.file = Some(fs::File::open("/dev/null").unwrap());
    let err = inc.finish().unwrap_err().to_string();
    assert!(err.contains("\"a\""), "{err}");
    assert!(!temp.exists(), "the temporary went with it");
    assert_eq!(fs::read(t.path().join("a")).unwrap(), b"old");
}
