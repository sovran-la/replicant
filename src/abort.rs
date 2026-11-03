//! The flag that stops a running transfer: raised by the lease watch when
//! home says the queue entry is gone, or by the local socket when the CLI
//! hangs up. Whoever runs the transfer polls it and stops; the first raise
//! says why.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// A flag that stops running transfers. Cloned into whatever runs them;
/// raised once, never lowered. It may say why ([`Abort::raise_because`]);
/// the first raise decides, so a reason never relabels an earlier stop.
#[derive(Debug, Clone, Default)]
pub struct Abort(Arc<Raised>);

#[derive(Debug, Default)]
struct Raised {
    flag: AtomicBool,
    why: Mutex<Option<String>>,
}

impl Abort {
    /// Raise it without a reason: a stopped transfer reads as a lost lease.
    pub fn raise(&self) {
        let _first = self.0.why.lock().unwrap_or_else(|e| e.into_inner());
        self.0.flag.store(true, Ordering::SeqCst);
    }
    /// Raise it, saying why, unless it was already raised.
    pub fn raise_because(&self, why: &str) {
        let mut first = self.0.why.lock().unwrap_or_else(|e| e.into_inner());
        if !self.0.flag.load(Ordering::SeqCst) {
            *first = Some(why.into());
        }
        self.0.flag.store(true, Ordering::SeqCst);
    }
    pub fn raised(&self) -> bool {
        self.0.flag.load(Ordering::SeqCst)
    }
    /// Why it was raised, if whoever raised it first said.
    pub fn why(&self) -> Option<String> {
        self.0.why.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}
