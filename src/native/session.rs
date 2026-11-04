//! One data connection's conversation, after [`super::frame::MAGIC`] (which
//! the dialer writes, and home's listener reads to tell it from a request).
//!
//! The leaf always dials and always drives: it says which unit and which
//! way in [`Hello`], and home answers [`Kind::Ready`] or [`Kind::Refused`].
//! On a push the leaf sends and home receives; on a pull the leaf asks for
//! entries and home sends them. Either way every frame is acked by its
//! receiver, and a side that gets no ack (or no frame) within the ack
//! timeout treats the connection as dead.

use super::conn::Conn;
use super::frame::{DATA_VERSION, Frame, Kind};
use crate::marker::Direction;
use crate::units::UnitKind;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// How long a peer may take to say [`Hello`], or to answer it.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(30);

/// The bounds home puts on the ack timeout a leaf asks for.
pub const ACK_TIMEOUT_BOUNDS: (Duration, Duration) =
    (Duration::from_millis(100), Duration::from_secs(600));

/// The leaf's first frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub version: u32,
    /// The leaf's fleet name. Not identity: home derives the host from the
    /// certificate and refuses a hello naming another.
    pub host: String,
    /// The sync's queue entry; home serves only its head, taken for a sync.
    pub lease: u64,
    pub rel: String,
    pub kind: UnitKind,
    pub is_file: bool,
    pub direction: Direction,
    /// How long either side waits for an ack before calling the connection
    /// dead (bounded by [`ACK_TIMEOUT_BOUNDS`] on home).
    pub ack_timeout_ms: u64,
}

impl Hello {
    /// The ack timeout home will use, within its bounds.
    pub fn ack_timeout(&self) -> Duration {
        let (lo, hi) = ACK_TIMEOUT_BOUNDS;
        Duration::from_millis(self.ack_timeout_ms).clamp(lo, hi)
    }
}

/// The leaf's side of the handshake: say hello, and wait for home's answer.
pub fn open(conn: &mut Conn, hello: &Hello) -> Result<()> {
    conn.set_timeout(HELLO_TIMEOUT.max(hello.ack_timeout()))?;
    conn.send(&Frame::json(Kind::Hello, hello)?)?;
    conn.recv()?.expect(Kind::Ready).map_err(|e| match e {
        Error::Transfer(why) => Error::Refused(format!("home refused the data connection: {why}")),
        e => e,
    })?;
    conn.set_timeout(hello.ack_timeout())
}

/// Home's side: read the hello (the magic line already consumed).
pub fn hello(conn: &mut Conn) -> Result<Hello> {
    conn.set_timeout(HELLO_TIMEOUT)?;
    let h: Hello = conn.recv()?.expect(Kind::Hello)?.parse()?;
    if h.version != DATA_VERSION {
        return Err(Error::Refused(format!(
            "data protocol {} spoken, this home speaks {DATA_VERSION}; run the same replicant on both",
            h.version
        )));
    }
    Ok(h)
}
