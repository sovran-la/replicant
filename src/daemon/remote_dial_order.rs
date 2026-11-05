//! The pure half of dialing: the order a name's addresses are tried in,
//! which candidate goes first on a redial, and the words for a miss.

use super::Miss;
use crate::Error;
use std::net::SocketAddr;

impl Miss {
    pub(super) fn detail(&self) -> &str {
        match self {
            Miss::Unreachable(d) => d,
            Miss::Refused(d) => d,
        }
    }

    /// The error for a dial that had only this address to try.
    pub(super) fn single(&self, address: &str, source: &str) -> Error {
        match self {
            Miss::Unreachable(d) => Error::Remote(format!("home at {address} is unreachable: {d}")),
            Miss::Refused(d) => Error::Tls(format!(
                "home at {address} refused the handshake with {source} ({d}). Is that home's \
                 current bundle?"
            )),
        }
    }
}

/// The order to try a name's addresses in: IPv4 before IPv6, and never an
/// IPv6 link-local address without a scope. The resolver hands those back
/// for mDNS names (`host.local`), but without a scope the kernel cannot pick
/// an interface, so the SYN goes nowhere and the dial sits out the whole
/// connect timeout before the IPv4 address even gets a turn.
pub(super) fn dial_order(addrs: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let mut v4 = Vec::new();
    let mut v6 = Vec::new();
    for a in addrs {
        match a {
            SocketAddr::V4(_) => v4.push(a),
            SocketAddr::V6(six) if is_link_local(six.ip()) && six.scope_id() == 0 => {}
            SocketAddr::V6(_) => v6.push(a),
        }
    }
    v4.extend(v6);
    v4
}

/// fe80::/10.
fn is_link_local(ip: &std::net::Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xffc0) == 0xfe80
}

/// What a name resolved to, in dial order, with `first` (the IP that
/// answered last time, if it is still among them) moved to the front.
pub(super) fn candidates(resolved: Vec<SocketAddr>, first: Option<SocketAddr>) -> Vec<SocketAddr> {
    let mut addrs = dial_order(resolved);
    if let Some(f) = first.filter(|f| addrs.contains(f)) {
        addrs.retain(|a| *a != f);
        addrs.insert(0, f);
    }
    addrs
}

/// The log line for a dial that answered at `entry` after `misses`, if there
/// were any: what was skipped and why, so a sync quietly on the slow path,
/// or something at the LAN address that is not home, leaves a trace.
pub(super) fn skipped_line(entry: &str, misses: &[(&String, Miss)]) -> Option<String> {
    if misses.is_empty() {
        return None;
    }
    Some(format!(
        "home answered at {entry}; skipped {}",
        misses
            .iter()
            .map(|(a, m)| format!("{a} ({})", m.detail()))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

#[cfg(test)]
#[path = "remote_dial_order_tests.rs"]
mod tests;
