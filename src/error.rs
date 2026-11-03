//! One error type for the crate.
//!
//! Every variant carries a sentence meant for the person at the terminal. The
//! variant says which part of replicant refused; the text says why and, where
//! there is one, what to do about it.

use std::fmt;

#[derive(Debug)]
pub enum Error {
    /// fleet.toml or leaf.toml is missing, malformed or contradicts itself.
    Config(String),
    /// A path failed normalization or escaped its bounds.
    Path(String),
    /// A filesystem operation failed.
    Io(std::io::Error),
    /// A git invocation failed or produced output this build cannot read.
    Git(String),
    /// A native transfer failed: a dead or stalled data connection, a
    /// frame that makes no sense, a file the receiver could not write.
    Transfer(String),
    /// Certificates, bundles or the TLS handshake.
    Tls(String),
    /// The other daemon could not be reached, or answered with an error.
    Remote(String),
    /// The local daemon is not running or refused the request.
    Daemon(String),
    /// Home refused the request on policy: a cap, a bound, the queue.
    Refused(String),
    /// A root or subscription base that should exist does not (review F4):
    /// never read as "empty", which would plan a whole-unit delete.
    Missing(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Config(m) => write!(f, "config: {m}"),
            Error::Path(m) => write!(f, "path: {m}"),
            Error::Io(e) => write!(f, "io: {e}"),
            Error::Git(m) => write!(f, "git: {m}"),
            Error::Transfer(m) => write!(f, "transfer: {m}"),
            Error::Tls(m) => write!(f, "tls: {m}"),
            Error::Remote(m) => write!(f, "remote: {m}"),
            Error::Daemon(m) => write!(f, "daemon: {m}"),
            Error::Refused(m) => write!(f, "refused: {m}"),
            Error::Missing(m) => write!(f, "missing: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
