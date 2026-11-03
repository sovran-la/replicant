//! A unit's sends, spread over its data connections: one worker per
//! connection, each taking the next item until none is left. An item is a
//! batch of small files and links, or one large file: a file is never
//! split across connections. The largest files go first, so the longest
//! sends start early and the batches fill in around them.

use super::conn::Conn;
use super::frame::{self, Frame, Kind};
use super::manifest::{Entry, EntryKind};
use super::receive::{Incoming, Receiver};
use super::send::{self, Sent, Window};
use super::source::Source;
use super::{BATCH_BYTES, SMALL};
use crate::abort::Abort;
use crate::{Error, Result};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Entries in one batch, at most: its header stays well under a frame.
const BATCH_ENTRIES: usize = 2048;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Batch(Vec<String>),
    Big(String),
}

/// The send list as items: large files by size, largest first; then the
/// rest in path order, batched.
pub fn items(send: &[Entry]) -> Vec<Item> {
    let mut big: Vec<&Entry> = send
        .iter()
        .filter(|e| e.kind == EntryKind::File && e.size >= SMALL)
        .collect();
    big.sort_by(|a, b| b.size.cmp(&a.size).then(a.rel.cmp(&b.rel)));
    let mut out: Vec<Item> = big.iter().map(|e| Item::Big(e.rel.clone())).collect();
    let (mut cur, mut bytes) = (Vec::new(), 0u64);
    for e in send
        .iter()
        .filter(|e| !(e.kind == EntryKind::File && e.size >= SMALL))
    {
        cur.push(e.rel.clone());
        bytes += e.size;
        if bytes >= BATCH_BYTES || cur.len() >= BATCH_ENTRIES {
            out.push(Item::Batch(std::mem::take(&mut cur)));
            bytes = 0;
        }
    }
    if !cur.is_empty() {
        out.push(Item::Batch(cur));
    }
    out
}

/// Whether `rel` is one of a repo's refs: `.git/HEAD`, `.git/packed-refs`,
/// or under `.git/refs/`. They go in a round of their own, after everything
/// else arrived, so a transfer stopped partway never leaves a ref pointing
/// at an object that did not arrive (nor a new repo with a `HEAD` and
/// nothing behind it).
pub fn is_ref(rel: &str) -> bool {
    let comps: Vec<&str> = rel.split('/').collect();
    comps.iter().position(|c| *c == ".git").is_some_and(|i| {
        matches!(&comps[i + 1..], ["HEAD"] | ["packed-refs"]) || comps.get(i + 1) == Some(&"refs")
    })
}

/// The send list in its two rounds: everything but refs, then refs.
pub fn rounds(send: &[Entry]) -> [Vec<Item>; 2] {
    let (refs, rest): (Vec<Entry>, Vec<Entry>) = send.iter().cloned().partition(|e| is_ref(&e.rel));
    [items(&rest), items(&refs)]
}

/// What the workers share: the round's items, the next one to take, and
/// what has been done in every round.
#[derive(Default)]
pub struct Shared {
    items: Mutex<Vec<Item>>,
    next: AtomicUsize,
    pub files: AtomicU64,
    pub bytes: AtomicU64,
    /// Entries renamed into place on the target, as sent.
    pub sent: Mutex<Vec<Entry>>,
    /// Entries left out because they changed or went.
    pub changed: Mutex<Vec<String>>,
}

impl Shared {
    pub fn new(items: Vec<Item>) -> Self {
        let sh = Self::default();
        sh.round(items);
        sh
    }

    /// Begin a round of `items` (the last one must be over).
    pub fn round(&self, items: Vec<Item>) {
        *lock(&self.items) = items;
        self.next.store(0, Ordering::SeqCst);
    }

    pub fn len(&self) -> usize {
        lock(&self.items).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn take(&self) -> Option<Item> {
        let i = self.next.fetch_add(1, Ordering::Relaxed);
        lock(&self.items).get(i).cloned()
    }

    fn done(&self, s: Sent) {
        match s {
            Sent::Whole(e) => {
                self.files.fetch_add(1, Ordering::Relaxed);
                if e.kind != EntryKind::File || e.size < SMALL {
                    self.bytes.fetch_add(e.size, Ordering::Relaxed);
                }
                lock(&self.sent).push(e);
            }
            Sent::Changed(rel) => lock(&self.changed).push(rel),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn stopped(abort: &Abort) -> Result<()> {
    match abort.raised() {
        true => Err(Error::Transfer(
            abort.why().unwrap_or_else(|| "the lease was lost".into()),
        )),
        false => Ok(()),
    }
}

/// A push worker: send each item to home, which acks it once written.
pub fn push(conn: &mut Conn, src: &Source<'_>, sh: &Shared, abort: &Abort) -> Result<()> {
    while let Some(item) = sh.take() {
        stopped(abort)?;
        match item {
            Item::Batch(rels) => {
                let (f, sent) = send::batch(src, &rels)?;
                if let Some(f) = f {
                    Window::new(conn).call(&f)?;
                }
                sent.into_iter().for_each(|s| sh.done(s));
            }
            Item::Big(rel) => {
                let s = send::file(&mut Window::counting(conn, &sh.bytes), src, &rel)?;
                sh.done(s);
            }
        }
    }
    Ok(())
}

/// A pull worker: ask home for each item and write what arrives, acking
/// every frame.
pub fn pull(conn: &mut Conn, rx: &Receiver<'_>, sh: &Shared, abort: &Abort) -> Result<()> {
    while let Some(item) = sh.take() {
        stopped(abort)?;
        let asked: Vec<String> = match item {
            Item::Batch(rels) => rels,
            Item::Big(rel) => vec![rel],
        };
        conn.send(&Frame::json(Kind::Get, &asked)?)?;
        let mut got: Vec<Entry> = Vec::new();
        let mut open: Option<(Entry, Incoming<'_, '_>)> = None;
        loop {
            let f = conn.recv()?;
            let kind = f.kind;
            match kind {
                Kind::Done => break,
                Kind::Batch => {
                    let (entries, bytes) = frame::unbatch(&f)?;
                    let mut at = 0usize;
                    for e in entries {
                        match e.kind {
                            EntryKind::File => {
                                let n = e.size as usize;
                                rx.file(&e, &bytes[at..at + n])?;
                                at += n;
                            }
                            _ => rx.symlink(&e)?,
                        }
                        got.push(e);
                    }
                }
                Kind::FileStart => {
                    let e: Entry = f.parse()?;
                    let inc = rx.begin(&e)?;
                    open = Some((e, inc));
                }
                Kind::Chunk => {
                    let (_, inc) = open
                        .as_mut()
                        .ok_or_else(|| Error::Transfer("home sent a chunk of no file".into()))?;
                    inc.write(&f.payload)?;
                    sh.bytes
                        .fetch_add(f.payload.len() as u64, Ordering::Relaxed);
                }
                Kind::FileEnd => {
                    let (e, inc) = open
                        .take()
                        .ok_or_else(|| Error::Transfer("home ended no file".into()))?;
                    inc.finish()?;
                    got.push(e);
                }
                Kind::FileDiscard => open = None,
                _ => {
                    f.expect(Kind::Done)?;
                }
            }
            if kind != Kind::Done {
                conn.send(&Frame::new(Kind::Ack, vec![]))?;
            }
        }
        for rel in &asked {
            match got.iter().position(|e| &e.rel == rel) {
                Some(i) => sh.done(Sent::Whole(got.swap_remove(i))),
                None => sh.done(Sent::Changed(rel.clone())),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "workers_tests.rs"]
mod tests;
