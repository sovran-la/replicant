//! Home sending what a pull asks for. The leaf drives: each
//! [`Kind::Get`] names entries; home answers with a batch of the small
//! ones, or one large file's start, chunks and end, every frame acked by
//! the leaf, and closes the answer with [`Kind::Done`]. An entry that is no
//! longer what the leaf's diff saw is left out; the leaf's verification
//! finds it. Home never sends what its policy excludes, nor anything
//! through a link.

use super::SMALL;
use super::conn::Conn;
use super::frame::{Frame, Kind};
use super::manifest::EntryKind;
use super::send::{self, Window};
use super::serve::Served;
use super::source::Source;
use crate::{Error, Result};

pub fn send(conn: &mut Conn, served: &Served, still: &dyn Fn() -> Result<()>) -> Result<()> {
    let src = Source::open(&served.root, served.is_file, Some(&served.policy))?;
    loop {
        let f = conn.recv()?;
        match f.kind {
            Kind::Done => return Ok(()),
            Kind::Get => {
                still()?;
                let rels: Vec<String> = f.parse()?;
                answer(conn, &src, &rels)?;
                conn.send(&Frame::new(Kind::Done, vec![]))?;
            }
            other => {
                return Err(Error::Transfer(format!(
                    "a {other:?} frame has no place in a pull"
                )));
            }
        }
    }
}

fn answer(conn: &mut Conn, src: &Source<'_>, rels: &[String]) -> Result<()> {
    if let [rel] = rels
        && let Some((_, e)) = src.now(rel)?
        && e.kind == EntryKind::File
        && e.size >= SMALL
    {
        send::file(&mut Window::new(conn), src, rel)?;
        return Ok(());
    }
    send::batch(src, rels, &mut |f, _| match f {
        Some(f) => Window::new(conn).call(&f).map(|_| ()),
        None => Ok(()),
    })
}
