//! Small JSON records kept one per unit under `state/`: baselines and markers.
//!
//! The file name is a hash of the unit path, so any path is a safe file name;
//! the path itself is inside the record, which is what [`Store::all`] reads.
//! Records carry a version and a record of an unknown version is an error,
//! not a guess (the agents crate's rule for anything that crosses vintages).

use crate::{Error, Result};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const RECORD_VERSION: u32 = 1;

/// Something stored one-per-unit.
pub trait Record: Serialize + DeserializeOwned {
    fn rel(&self) -> &str;
    fn version(&self) -> u32;
}

/// A directory of records.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn file(&self, rel: &str) -> PathBuf {
        let h = Sha256::digest(rel.as_bytes());
        let name: String = h.iter().take(16).map(|b| format!("{b:02x}")).collect();
        self.dir.join(format!("{name}.json"))
    }

    pub fn get<R: Record>(&self, rel: &str) -> Result<Option<R>> {
        let path = self.file(rel);
        match std::fs::read_to_string(&path) {
            Ok(text) => parse(&text, &path).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn all<R: Record>(&self) -> Result<Vec<R>> {
        let mut out = Vec::new();
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        for item in entries {
            let path = item?.path();
            if path.extension().is_some_and(|e| e == "json") {
                out.push(parse::<R>(&std::fs::read_to_string(&path)?, &path)?);
            }
        }
        out.sort_by(|a, b| a.rel().cmp(b.rel()));
        Ok(out)
    }

    /// Write a record: contacts, and from v0.2 on (`sync`'s apply)
    /// baselines and markers.
    pub fn put<R: Record>(&self, r: &R) -> Result<()> {
        let text = serde_json::to_string_pretty(r)
            .map_err(|e| Error::Config(format!("could not serialize a record: {e}")))?;
        crate::config::write_atomic(&self.file(r.rel()), &text)
    }
}

impl Store {
    /// Remove `rel`'s record, if there is one.
    pub fn delete(&self, rel: &str) -> Result<()> {
        match std::fs::remove_file(self.file(rel)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

fn parse<R: Record>(text: &str, path: &Path) -> Result<R> {
    let r: R = serde_json::from_str(text)
        .map_err(|e| Error::Config(format!("{} is unreadable: {e}", path.display())))?;
    if r.version() != RECORD_VERSION {
        return Err(Error::Config(format!(
            "{} is record version {}, and this build reads {RECORD_VERSION}",
            path.display(),
            r.version()
        )));
    }
    Ok(r)
}
