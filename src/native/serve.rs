//! Home's side of a data connection, once its hello was admitted: receive
//! a push, or send what a pull asks for, until the leaf says it is done.

use super::conn::Conn;
use super::frame::Kind;
use crate::exclude::Excludes;
use crate::marker::Direction;
use crate::{Error, Result};
use std::path::PathBuf;

/// What one admitted connection may touch.
#[derive(Debug, Clone)]
pub struct Served {
    pub host: String,
    pub direction: Direction,
    /// The unit's path on home.
    pub root: PathBuf,
    pub is_file: bool,
    /// What home never sends or writes, whatever the leaf asks.
    pub policy: Excludes,
    /// A push: where replaced and deleted paths are renamed to (this run's
    /// backup directory).
    pub backup: Option<PathBuf>,
}

/// Serve frames until [`Kind::Done`]. `still` fails once the lease is no
/// longer at the head of home's queue: checked before every frame that
/// writes or sends.
pub fn answer(conn: &mut Conn, served: &Served, still: &dyn Fn() -> Result<()>) -> Result<()> {
    let _ = served;
    match conn.recv()?.kind {
        Kind::Done => Ok(()),
        other => {
            still()?;
            Err(Error::Transfer(format!(
                "a {other:?} frame is not something home serves"
            )))
        }
    }
}
