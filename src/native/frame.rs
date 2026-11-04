//! Data-connection frames: `u32` payload length (big-endian), `u8` kind,
//! payload. Control-ish payloads are JSON; file bytes are raw.
//!
//! A data connection starts with [`MAGIC`] on a line of its own, where a
//! control connection's first line is a JSON request, so home's listener
//! tells the two apart by the first byte. Then the leaf sends
//! [`Kind::Hello`] and home answers [`Kind::Ready`] or [`Kind::Refused`].

use super::manifest::{Entry, EntryKind};
use crate::{Error, Result};
use serde::{Serialize, de::DeserializeOwned};
use std::io::{Read, Write};

/// The first line of a data connection.
pub const MAGIC: &[u8] = b"replicant-data\n";

/// The data protocol's version, carried in [`Kind::Hello`].
pub const DATA_VERSION: u32 = 1;

/// A payload larger than this is refused rather than buffered: a chunk is
/// [`super::CHUNK`], a batch about [`super::BATCH_BYTES`] of files and at most
/// that of header, a list [`LIST_BYTES`].
pub const MAX_PAYLOAD: usize = 8 << 20;

/// A list goes in frames of about this many bytes ([`lists`]). Each is acked
/// once its whole list is done, within the ack timeout, so they stay short.
pub const LIST_BYTES: usize = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    /// Leaf → home, JSON [`super::session::Hello`]: which unit, which way.
    Hello = 1,
    /// Home → leaf: go ahead (empty).
    Ready = 2,
    /// Home → leaf, JSON string: why not. The connection then closes.
    Refused = 3,
    /// The receiver took the frame (empty, or JSON for [`Kind::Deletes`]).
    Ack = 4,
    /// The receiver could not take the frame, JSON string: why. Ends the
    /// connection's work.
    Fail = 5,
    /// Push: temporary files a dead transfer left (JSON paths).
    Clean = 10,
    /// Push: target paths whose kind changes, with the deletions under them
    /// (JSON).
    Replace = 11,
    /// Push: directories to create or chmod (JSON entries).
    Mkdirs = 12,
    /// Small files and links: `u32` header length, JSON entries, then each
    /// file's bytes in order.
    Batch = 13,
    /// A large file begins (JSON entry).
    FileStart = 14,
    /// The next bytes of the file begun (raw).
    Chunk = 15,
    /// The file begun is complete: commit it (empty).
    FileEnd = 16,
    /// The file begun changed while it was read: drop it (empty).
    FileDiscard = 17,
    /// Push: delete these (JSON `(path, is_dir)`); acked with what stayed.
    Deletes = 18,
    /// Push: final directory modes, deepest first (JSON entries).
    DirModes = 19,
    /// Pull: send these entries (JSON). Home answers with a batch, or a
    /// file's start, chunks and end, each acked by the leaf.
    Get = 20,
    /// Nothing more on this connection (empty).
    Done = 30,
}

impl Kind {
    fn from_u8(b: u8) -> Option<Self> {
        use Kind::*;
        Some(match b {
            1 => Hello,
            2 => Ready,
            3 => Refused,
            4 => Ack,
            5 => Fail,
            10 => Clean,
            11 => Replace,
            12 => Mkdirs,
            13 => Batch,
            14 => FileStart,
            15 => Chunk,
            16 => FileEnd,
            17 => FileDiscard,
            18 => Deletes,
            19 => DirModes,
            20 => Get,
            30 => Done,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub kind: Kind,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(kind: Kind, payload: Vec<u8>) -> Self {
        Self { kind, payload }
    }

    pub fn json<T: Serialize>(kind: Kind, v: &T) -> Result<Self> {
        let payload = serde_json::to_vec(v).map_err(|e| Error::Transfer(format!("encode: {e}")))?;
        Ok(Self { kind, payload })
    }

    /// The payload as JSON.
    pub fn parse<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_slice(&self.payload)
            .map_err(|e| Error::Transfer(format!("unreadable {:?} frame: {e}", self.kind)))
    }

    /// This frame must be of `kind`; a [`Kind::Fail`] or [`Kind::Refused`]
    /// says why it is not.
    pub fn expect(self, kind: Kind) -> Result<Self> {
        match self.kind {
            k if k == kind => Ok(self),
            Kind::Fail | Kind::Refused => {
                let why: String = self.parse().unwrap_or_else(|e| e.to_string());
                Err(Error::Transfer(why))
            }
            other => Err(Error::Transfer(format!(
                "expected a {kind:?} frame, got {other:?}"
            ))),
        }
    }
}

/// `items` as JSON-array frames of `kind`, in order, each about
/// [`LIST_BYTES`] at most. Each frame stands alone: nothing depends on
/// where a list is split. An empty list is no frames.
pub fn lists<T: Serialize>(kind: Kind, items: &[T]) -> Result<Vec<Frame>> {
    let mut frames = Vec::new();
    let mut cur = vec![b'['];
    for item in items {
        let j = serde_json::to_vec(item).map_err(|e| Error::Transfer(format!("encode: {e}")))?;
        if cur.len() > 1 && cur.len() + j.len() + 2 > LIST_BYTES {
            cur.push(b']');
            frames.push(Frame::new(kind, std::mem::replace(&mut cur, vec![b'['])));
        }
        if cur.len() > 1 {
            cur.push(b',');
        }
        cur.extend_from_slice(&j);
    }
    if cur.len() > 1 {
        cur.push(b']');
        frames.push(Frame::new(kind, cur));
    }
    Ok(frames)
}

/// A [`Kind::Batch`] of `entries`, whose files' bytes are `bytes`, in order.
pub fn batch(entries: &[Entry], bytes: &[u8]) -> Result<Frame> {
    let head = serde_json::to_vec(entries).map_err(|e| Error::Transfer(format!("encode: {e}")))?;
    let mut payload = Vec::with_capacity(4 + head.len() + bytes.len());
    payload.extend_from_slice(&(head.len() as u32).to_be_bytes());
    payload.extend_from_slice(&head);
    payload.extend_from_slice(bytes);
    Ok(Frame::new(Kind::Batch, payload))
}

/// A batch's entries and their bytes, which must be exactly the files'
/// sizes summed.
pub fn unbatch(f: &Frame) -> Result<(Vec<Entry>, &[u8])> {
    let bad = |why: &str| Error::Transfer(format!("a malformed batch: {why}"));
    let p = &f.payload;
    let n = p
        .get(..4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
        .ok_or_else(|| bad("no header length"))?;
    let head = p.get(4..4 + n).ok_or_else(|| bad("header past its end"))?;
    let entries: Vec<Entry> =
        serde_json::from_slice(head).map_err(|e| bad(&format!("header: {e}")))?;
    let bytes = &p[4 + n..];
    let want = entries
        .iter()
        .filter(|e| e.kind == EntryKind::File)
        .try_fold(0u64, |sum, e| sum.checked_add(e.size));
    if want != Some(bytes.len() as u64) {
        return Err(bad(&format!(
            "its files are {} bytes, it carries {}",
            want.map_or("too many".to_string(), |w| w.to_string()),
            bytes.len()
        )));
    }
    Ok((entries, bytes))
}

/// Write one frame. One past [`MAX_PAYLOAD`] is an error, nothing written:
/// the other side would refuse it, and frames are sized from what is on disk
/// when they are built, which nothing here controls.
pub fn write<W: Write + ?Sized>(w: &mut W, f: &Frame) -> std::io::Result<()> {
    if f.payload.len() > MAX_PAYLOAD {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "a {:?} frame of {} bytes is larger than the limit",
                f.kind,
                f.payload.len()
            ),
        ));
    }
    let mut head = [0u8; 5];
    head[..4].copy_from_slice(&(f.payload.len() as u32).to_be_bytes());
    head[4] = f.kind as u8;
    w.write_all(&head)?;
    w.write_all(&f.payload)?;
    w.flush()
}

/// Read one frame. End of stream before a frame starts is an error naming
/// the peer's departure; a length past [`MAX_PAYLOAD`] or an unknown kind
/// is refused.
pub fn read<R: Read + ?Sized>(r: &mut R) -> Result<Frame> {
    let mut head = [0u8; 5];
    r.read_exact(&mut head).map_err(io_error)?;
    let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
    if len > MAX_PAYLOAD {
        return Err(Error::Transfer(format!(
            "a frame of {len} bytes is larger than the limit"
        )));
    }
    let kind = Kind::from_u8(head[4])
        .ok_or_else(|| Error::Transfer(format!("unknown frame kind {}", head[4])))?;
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload).map_err(io_error)?;
    Ok(Frame { kind, payload })
}

/// An I/O failure on a data connection, said the way a person needs it.
pub fn io_error(e: std::io::Error) -> Error {
    use std::io::ErrorKind::*;
    match e.kind() {
        WouldBlock | TimedOut => Error::Transfer(
            "no answer from the other side within the ack timeout; the connection is dead".into(),
        ),
        UnexpectedEof => Error::Transfer("the other side closed the data connection".into()),
        _ => Error::Transfer(format!("data connection: {e}")),
    }
}

#[cfg(test)]
#[path = "frame_tests.rs"]
mod tests;
