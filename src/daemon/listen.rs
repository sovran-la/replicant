//! Home's mTLS listener: one thread per connection, which is a control
//! request (one JSON line, one answer) or a data connection (the magic
//! line, then frames), told apart by the first byte.

use super::{certs, home, remote, wire};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

pub(super) fn spawn_tcp(
    l: Arc<remote::MtlsListener>,
    home: Arc<home::Home>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        loop {
            let conn = l.accept();
            if stop.load(Ordering::SeqCst) {
                break;
            }
            let Ok(tcp) = conn else { continue };
            let (l, home) = (l.clone(), home.clone());
            std::thread::spawn(move || {
                let mut s = match l.secure(tcp) {
                    Ok(s) => s,
                    Err(e) => return eprintln!("replicant: refused a connection: {e}"),
                };
                // Mandatory client auth: a finished handshake has a peer
                // certificate, and home decides who it is by its fingerprint.
                let peer = s
                    .conn
                    .peer_certificates()
                    .and_then(|c| c.first())
                    .map(|c| certs::fingerprint(c));
                // A control request is one JSON line; a data connection
                // starts with its magic line instead.
                let mut first = [0u8; 1];
                if std::io::Read::read_exact(&mut s, &mut first).is_err() {
                    return;
                }
                if first[0] != b'{' {
                    return serve_data(&home, s, first[0], peer);
                }
                let line = std::io::Read::chain(&first[..], &mut s);
                let answer = match (remote::recv::<_, wire::HomeCall>(line), peer) {
                    (Ok(call), Some(peer)) => {
                        home.handle_from(call, &peer, &|| remote::connected(&s.sock))
                    }
                    (Ok(_), None) => wire::HomeAnswer {
                        result: Err("no client certificate".into()),
                        journal: vec![],
                    },
                    (Err(e), _) => wire::HomeAnswer {
                        result: Err(e.to_string()),
                        journal: vec![],
                    },
                };
                let _ = remote::send(&mut s, &answer);
                s.conn.send_close_notify();
                let _ = s.conn.complete_io(&mut s.sock);
            });
        }
    })
}

/// A data connection: the rest of its magic line, then home serves it.
fn serve_data(home: &home::Home, mut s: remote::ServerStream, first: u8, peer: Option<String>) {
    let magic = crate::native::frame::MAGIC;
    let mut rest = vec![0u8; magic.len() - 1];
    let ok = first == magic[0] && std::io::Read::read_exact(&mut s, &mut rest).is_ok();
    let (Some(peer), true) = (peer, ok && rest == magic[1..]) else {
        return eprintln!("replicant: refused a connection that is neither a request nor data");
    };
    let Ok(sock) = s.sock.try_clone() else {
        return;
    };
    let conn = crate::native::conn::Conn::new(Box::new(s), crate::native::conn::Sock::Tcp(sock));
    home.serve_data(&home::Caller::Cert(peer), conn);
}
