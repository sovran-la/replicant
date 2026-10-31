//! The empty directory an absent source stands in with.
//!
//! Made with mkdtemp(3) under the config dir: a fresh, uniquely named, 0700
//! directory every time, never one that already existed. A predictable name in
//! a shared `/tmp` let whatever was planted there (a directory, or a symlink to
//! one) become the source of a `--delete`.

use crate::Result;
use std::ffi::{CString, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

/// A directory that exists, is empty, and is removed on drop.
#[derive(Debug)]
pub(super) struct EmptyDir(PathBuf);

impl EmptyDir {
    pub fn new(scratch: &Path) -> Result<Self> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(scratch)?;
        let template = scratch.join("empty-XXXXXX");
        let mut bytes = CString::new(template.as_os_str().as_bytes())
            .map_err(std::io::Error::other)?
            .into_bytes_with_nul();
        // SAFETY: `bytes` is a NUL-terminated, writable buffer that outlives
        // the call; mkdtemp rewrites the X's in place and nothing else.
        let made = unsafe { libc::mkdtemp(bytes.as_mut_ptr().cast()) };
        if made.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        bytes.pop();
        Ok(Self(PathBuf::from(OsString::from_vec(bytes))))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for EmptyDir {
    fn drop(&mut self) {
        // remove_dir, not remove_dir_all: it was empty, and if something
        // appeared in it, leave it for a human rather than delete it.
        let _ = std::fs::remove_dir(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn unique_private_empty_and_removed_on_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let scratch = tmp.path().join("cfg/tmp");
        let a = EmptyDir::new(&scratch).unwrap();
        let b = EmptyDir::new(&scratch).unwrap();
        assert_ne!(a.path(), b.path());
        for d in [&a, &b] {
            assert_eq!(d.path().parent(), Some(scratch.as_path()));
            let m = std::fs::symlink_metadata(d.path()).unwrap();
            assert!(m.is_dir() && !m.file_type().is_symlink());
            assert_eq!(m.permissions().mode() & 0o777, 0o700);
            assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0);
        }
        let pa = a.path().to_path_buf();
        drop(a);
        assert!(!pa.exists());
        assert!(b.path().exists());
    }

    #[test]
    fn never_reuses_what_is_already_there() {
        let tmp = tempfile::tempdir().unwrap();
        let scratch = tmp.path().join("tmp");
        std::fs::create_dir_all(&scratch).unwrap();
        let planted = tmp.path().join("planted");
        crate::testutil::write(&planted.join("victim"), "x");
        // Anything matching the template is skipped, never adopted.
        std::os::unix::fs::symlink(&planted, scratch.join("empty-AAAAAA")).unwrap();
        for _ in 0..20 {
            let d = EmptyDir::new(&scratch).unwrap();
            assert_ne!(d.path(), scratch.join("empty-AAAAAA"));
            assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0);
        }
    }

    #[test]
    fn unusable_scratch_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("file");
        std::fs::write(&file, "").unwrap();
        assert!(EmptyDir::new(&file).is_err());
    }
}
