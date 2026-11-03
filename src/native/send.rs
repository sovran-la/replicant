//! Sending frames that are acked: up to [`WINDOW`] may be unanswered at
//! once, so a large file streams without waiting a round trip per chunk,
//! and the sender still learns, frame by frame, what the receiver took.
//! Acks come back in order; waiting longer than the ack timeout for one
//! fails with the connection called dead (the socket's timeout).

use super::CHUNK;
use super::conn::Conn;
use super::frame::{self, Frame, Kind};
use super::manifest::Entry;
use super::source::{self, Read, Source};
use crate::Result;
use std::collections::VecDeque;
use std::io::Read as _;
use std::sync::atomic::{AtomicU64, Ordering};

/// Frames that may be in flight, unacked, on one connection.
pub const WINDOW: usize = 8;

pub struct Window<'c> {
    conn: &'c mut Conn,
    /// Bytes of file data in each unacked frame, oldest first.
    flight: VecDeque<u64>,
    /// Where acked file bytes are counted, if anywhere.
    acked: Option<&'c AtomicU64>,
}

impl<'c> Window<'c> {
    pub fn new(conn: &'c mut Conn) -> Self {
        Self {
            conn,
            flight: VecDeque::new(),
            acked: None,
        }
    }

    /// Count every acked byte of file data into `acked` (live progress).
    pub fn counting(conn: &'c mut Conn, acked: &'c AtomicU64) -> Self {
        Self {
            acked: Some(acked),
            ..Self::new(conn)
        }
    }

    /// Send `f`, carrying `bytes` of file data, once fewer than [`WINDOW`]
    /// frames are unacked.
    pub fn send(&mut self, f: &Frame, bytes: u64) -> Result<()> {
        while self.flight.len() >= WINDOW {
            self.take()?;
        }
        self.conn.send(f)?;
        self.flight.push_back(bytes);
        Ok(())
    }

    /// The oldest unacked frame's ack.
    fn take(&mut self) -> Result<Frame> {
        let ack = self.conn.recv()?.expect(Kind::Ack)?;
        let bytes = self.flight.pop_front().unwrap_or(0);
        if let Some(a) = self.acked {
            a.fetch_add(bytes, Ordering::Relaxed);
        }
        Ok(ack)
    }

    /// Wait until every frame sent is acked; the last ack.
    pub fn drain(&mut self) -> Result<Option<Frame>> {
        let mut last = None;
        while !self.flight.is_empty() {
            last = Some(self.take()?);
        }
        Ok(last)
    }

    /// Send `f` and wait for everything, its ack included.
    pub fn call(&mut self, f: &Frame) -> Result<Frame> {
        self.send(f, 0)?;
        Ok(self.drain()?.expect("one frame was sent"))
    }
}

/// What a send did with one entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sent {
    /// Acked: renamed into place on the receiver.
    Whole(Entry),
    /// Not there as it was, or changed while read: not sent.
    Changed(String),
}

/// One large file: its start, its chunks, its end (or a discard, if it
/// changed while it was read), every frame acked.
pub fn file(w: &mut Window<'_>, src: &Source<'_>, rel: &str) -> Result<Sent> {
    let Some((abs, e)) = src.now(rel)? else {
        return Ok(Sent::Changed(rel.into()));
    };
    let Some(mut f) = source::open(&abs).filter(|f| source::unchanged(f, &e)) else {
        return Ok(Sent::Changed(rel.into()));
    };
    w.send(&Frame::json(Kind::FileStart, &e)?, 0)?;
    let mut buf = vec![0u8; CHUNK];
    let mut read = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        read += n as u64;
        if read > e.size {
            break;
        }
        w.send(&Frame::new(Kind::Chunk, buf[..n].to_vec()), n as u64)?;
    }
    if read != e.size || !source::unchanged(&f, &e) {
        w.send(&Frame::new(Kind::FileDiscard, vec![]), 0)?;
        w.drain()?;
        return Ok(Sent::Changed(rel.into()));
    }
    w.send(&Frame::new(Kind::FileEnd, vec![]), 0)?;
    w.drain()?;
    Ok(Sent::Whole(e))
}

/// Small files and links in one [`Kind::Batch`], read as they are now.
/// The changed ones are left out; `None` when none is left.
pub fn batch(src: &Source<'_>, rels: &[String]) -> Result<(Option<Frame>, Vec<Sent>)> {
    let (mut entries, mut bytes, mut sent) = (Vec::new(), Vec::new(), Vec::new());
    for rel in rels {
        match src.read(rel)? {
            Read::Whole(e, b) => {
                bytes.extend_from_slice(&b);
                sent.push(Sent::Whole(e.clone()));
                entries.push(e);
            }
            Read::Changed => sent.push(Sent::Changed(rel.clone())),
        }
    }
    let f = match entries.is_empty() {
        true => None,
        false => Some(frame::batch(&entries, &bytes)?),
    };
    Ok((f, sent))
}

#[cfg(test)]
#[path = "send_tests.rs"]
mod tests;
