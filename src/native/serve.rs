//! Home's side of a data connection, once its hello was admitted: receive
//! a push, or send what a pull asks for, until the leaf says it is done.
//!
//! Every frame the leaf sends is answered, in order: [`Kind::Ack`] once it
//! is done with (a file's end: once the file is renamed into place), or
//! [`Kind::Fail`] with why, which ends the connection. Before every frame
//! that writes or reads the tree, home checks that the lease is still the
//! head of its queue.

use super::backup::Backup;
use super::conn::Conn;
use super::frame::{self, Frame, Kind};
use super::manifest::{Entry, EntryKind};
use super::receive::{Incoming, Receiver};
use crate::exclude::Excludes;
use crate::marker::Direction;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
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

/// A path whose kind changes, and what under it goes first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replace {
    pub rel: String,
    pub kind: EntryKind,
    #[serde(default)]
    pub under: Vec<(String, bool)>,
}

pub fn ack(conn: &mut Conn) -> Result<()> {
    conn.send(&Frame::new(Kind::Ack, vec![]))
}

/// Serve frames until [`Kind::Done`]. `still` fails once the lease is no
/// longer at the head of home's queue.
pub fn answer(conn: &mut Conn, served: &Served, still: &dyn Fn() -> Result<()>) -> Result<()> {
    match served.direction {
        Direction::Push => receive(conn, served, still),
        Direction::Pull => match conn.recv()?.kind {
            Kind::Done => Ok(()),
            other => Err(Error::Transfer(format!(
                "a {other:?} frame: home does not send yet"
            ))),
        },
    }
}

fn receive(conn: &mut Conn, served: &Served, still: &dyn Fn() -> Result<()>) -> Result<()> {
    let rx = Receiver::open(
        &served.root,
        served.is_file,
        Some(&served.policy),
        served.backup.clone().map(Backup::new),
    )?;
    let mut open: Option<Incoming<'_, '_>> = None;
    loop {
        let f = conn.recv()?;
        if f.kind == Kind::Done {
            rx.sync_dirs()?;
            return Ok(());
        }
        still()?;
        let reply = match f.kind {
            Kind::Clean => rx.clean(&f.parse::<Vec<String>>()?).map(|()| None),
            Kind::Replace => f.parse::<Vec<Replace>>().and_then(|list| {
                list.iter()
                    .try_for_each(|r| rx.replace(&r.rel, r.kind, &r.under))
                    .map(|()| None)
            }),
            Kind::Mkdirs => f
                .parse::<Vec<Entry>>()
                .and_then(|list| list.iter().try_for_each(|e| rx.mkdir(e)).map(|()| None)),
            Kind::Batch => batch(&rx, &f).map(|()| None),
            Kind::FileStart => {
                open = Some(rx.begin(&f.parse()?)?);
                Ok(None)
            }
            Kind::Chunk => match open.as_mut() {
                Some(inc) => inc.write(&f.payload).map(|()| None),
                None => Err(Error::Transfer("a chunk with no file begun".into())),
            },
            Kind::FileEnd => match open.take() {
                Some(inc) => inc.finish().map(|()| None),
                None => Err(Error::Transfer("a file's end with no file begun".into())),
            },
            Kind::FileDiscard => {
                open = None;
                Ok(None)
            }
            Kind::Deletes => rx
                .delete(&f.parse::<Vec<(String, bool)>>()?)
                .and_then(|kept| Frame::json(Kind::Ack, &kept).map(Some)),
            Kind::DirModes => rx.dir_modes(&f.parse::<Vec<Entry>>()?).map(|()| None),
            other => Err(Error::Transfer(format!(
                "a {other:?} frame has no place in a push"
            ))),
        }?;
        match reply {
            Some(r) => conn.send(&r)?,
            None => ack(conn)?,
        }
    }
}

/// Every file and link in a batch, in order.
fn batch(rx: &Receiver<'_>, f: &Frame) -> Result<()> {
    let (entries, bytes) = frame::unbatch(f)?;
    let mut at = 0usize;
    for e in &entries {
        match e.kind {
            EntryKind::File => {
                let n = e.size as usize;
                rx.file(e, &bytes[at..at + n])?;
                at += n;
            }
            EntryKind::Symlink => rx.symlink(e)?,
            EntryKind::Dir => {
                return Err(Error::Transfer(format!(
                    "{:?}: a directory in a batch",
                    e.rel
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "serve_tests.rs"]
mod tests;
