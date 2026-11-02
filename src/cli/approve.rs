//! `replicant approve`: the interactive flow on home. PLAN.md, "Enrolling a
//! new machine". It forces the one decision that matters — whether this box
//! gets a cap — and echoes the normalized cap before writing it, so what is
//! approved is what was seen, not what was typed.
//!
//! I/O is passed in so the flow is tested with scripted input.

use crate::daemon::wire::{LocalReply, LocalRequest};
use crate::timefmt;
use crate::{Error, Result};
use std::io::{BufRead, Write};

/// Read one line; `None` at end of input.
fn ask(input: &mut dyn BufRead, out: &mut dyn Write, prompt: &str) -> Result<Option<String>> {
    write!(out, "{prompt}")?;
    out.flush()?;
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        writeln!(out)?;
        return Ok(None);
    }
    Ok(Some(line.trim().to_string()))
}

pub fn run(
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    only: Option<&str>,
    call: &dyn Fn(LocalRequest) -> Result<LocalReply>,
) -> Result<()> {
    let LocalReply::Pending { hosts } = call(LocalRequest::Pending)? else {
        return Err(Error::Daemon("unexpected reply to pending".into()));
    };
    if let Some(h) = only.filter(|h| !hosts.iter().any(|p| p.host == *h)) {
        return reissue(input, out, h, call);
    }
    if hosts.is_empty() {
        writeln!(out, "Nothing pending.")?;
        return Ok(());
    }
    let now = crate::now_secs();
    writeln!(out, "Pending:")?;
    for p in &hosts {
        writeln!(
            out,
            "  {:<10} {}   wants: {}   ({} ago)",
            p.host,
            p.home_dir.display(),
            p.paths.join(", "),
            timefmt::ago(now.saturating_sub(p.requested_at))
        )?;
    }
    for p in hosts.iter().filter(|p| only.is_none_or(|h| h == p.host)) {
        writeln!(out)?;
        let yes = ask(input, out, &format!("Approve {}? [y/N] ", p.host))?;
        if !matches!(yes.as_deref(), Some("y" | "Y" | "yes")) {
            writeln!(out, "Skipped {}.", p.host)?;
            continue;
        }
        let (raw, preview) = loop {
            let Some(raw) = ask(
                input,
                out,
                "Allow list (relative to home dir, space-separated, empty = anything):\n> ",
            )?
            else {
                writeln!(out, "Not approved.")?;
                return Ok(());
            };
            match call(LocalRequest::PreviewAllow {
                host: p.host.clone(),
                raw: vec![raw.clone()],
            }) {
                Ok(LocalReply::AllowPreview(pv)) => break (raw, pv),
                Ok(other) => return Err(Error::Daemon(format!("unexpected reply: {other:?}"))),
                Err(e) => writeln!(out, "  {e}")?,
            }
        };
        match &preview.allow {
            Some(a) => writeln!(out, "  → {}", a.join(", "))?,
            None => writeln!(out, "  → (no cap: anything home serves)")?,
        }
        if !preview.drop.is_empty() {
            writeln!(
                out,
                "  drops from its request: {} (outside the cap)",
                preview.drop.join(", ")
            )?;
        }
        let confirm = ask(input, out, "Confirm? [Y/n] ")?;
        if confirm.is_none() || matches!(confirm.as_deref(), Some("n" | "N" | "no")) {
            writeln!(out, "Not approved.")?;
            continue;
        }
        let LocalReply::Approved(a) = call(LocalRequest::Approve {
            host: p.host.clone(),
            allow: vec![raw],
        })?
        else {
            return Err(Error::Daemon("unexpected reply to approve".into()));
        };
        writeln!(out, "Approved. Run `replicant sync` on {} to pull.", p.host)?;
        writeln!(
            out,
            "  Its certificate ({}) waits here; {} collects it on its next request.",
            a.cert, p.host
        )?;
    }
    Ok(())
}

/// `approve <host>` for a host that is not pending: issue an enrolled host a
/// new certificate (a lost `host.pem`, or a fleet upgraded from v0.1).
fn reissue(
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    host: &str,
    call: &dyn Fn(LocalRequest) -> Result<LocalReply>,
) -> Result<()> {
    let LocalReply::Fleet(f) = call(LocalRequest::Fleet)? else {
        return Err(Error::Daemon("unexpected reply to fleet".into()));
    };
    if !f.hosts.iter().any(|h| h.name == host) {
        return Err(Error::Refused(format!("{host} is not pending or enrolled")));
    }
    let yes = ask(
        input,
        out,
        &format!(
            "{host} is enrolled. Issue it a new certificate? Any certificate it has now stops \
             working. [y/N] "
        ),
    )?;
    if !matches!(yes.as_deref(), Some("y" | "Y" | "yes")) {
        writeln!(out, "Not reissued.")?;
        return Ok(());
    }
    let LocalReply::Reissued(r) = call(LocalRequest::Reissue { host: host.into() })? else {
        return Err(Error::Daemon("unexpected reply to reissue".into()));
    };
    writeln!(
        out,
        "Issued {host} a new certificate ({}); it collects it on its next request.",
        r.cert
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::home::Home;
    use crate::daemon::wire::HomeRequest;
    use crate::plan::HomeLink;
    use crate::testutil::{Bed, InProcess};
    use std::os::unix::fs::PermissionsExt;

    fn local(home: &Home) -> impl Fn(LocalRequest) -> Result<LocalReply> + '_ {
        move |req| match req {
            LocalRequest::Pending => Ok(LocalReply::Pending {
                hosts: home.pending()?,
            }),
            LocalRequest::PreviewAllow { host, raw } => {
                Ok(LocalReply::AllowPreview(home.preview_allow(&host, &raw)?))
            }
            LocalRequest::Approve { host, allow } => {
                Ok(LocalReply::Approved(home.approve(&host, &allow)?))
            }
            LocalRequest::Fleet => Ok(LocalReply::Fleet(home.fleet_state()?)),
            LocalRequest::Reissue { host } => Ok(LocalReply::Reissued(home.reissue(&host)?)),
            other => panic!("unexpected {other:?}"),
        }
    }

    fn pending_twinoaks(bed: &Bed, home: &Home) {
        let link = InProcess {
            home,
            host: "twinoaks".into(),
            home_dir: bed.leaf_hd.clone(),
        };
        link.call(HomeRequest::Subscribe {
            paths: vec![
                "~/work/tundra".into(),
                "~/work/sequoia".into(),
                "~/work/rust".into(),
            ],
        })
        .unwrap();
    }

    fn drive(home: &Home, script: &str, only: Option<&str>) -> (Result<()>, String) {
        let mut input = std::io::Cursor::new(script.as_bytes().to_vec());
        let mut out = Vec::new();
        let r = run(&mut input, &mut out, only, &local(home));
        (r, String::from_utf8(out).unwrap())
    }

    #[test]
    fn approve_with_a_cap_echoes_the_normalized_list_and_writes_it() {
        let bed = Bed::new("");
        let home = bed.home();
        pending_twinoaks(&bed, &home);
        let typed = format!("{}/work/sequoia ~/work/tundra", bed.leaf_hd.display());
        let (r, out) = drive(&home, &format!("y\n{typed}\ny\n"), None);
        r.unwrap();
        assert!(out.contains("Pending:\n  twinoaks"), "{out}");
        assert!(
            out.contains("wants: work/tundra, work/sequoia, work/rust"),
            "{out}"
        );
        assert!(out.contains("  → work/sequoia, work/tundra\n"), "{out}");
        assert!(out.contains("drops from its request: work/rust"), "{out}");
        assert!(
            out.contains("Approved. Run `replicant sync` on twinoaks to pull."),
            "{out}"
        );
        let f = crate::config::Fleet::load(&bed.home_dirs.fleet()).unwrap();
        assert_eq!(
            f.hosts["twinoaks"].allow,
            Some(vec!["work/sequoia".to_string(), "work/tundra".into()])
        );
    }

    #[test]
    fn empty_allow_is_no_cap() {
        let bed = Bed::new("");
        let home = bed.home();
        pending_twinoaks(&bed, &home);
        let (r, out) = drive(&home, "y\n\n\n", None);
        r.unwrap();
        assert!(out.contains("(no cap"), "{out}");
        let f = crate::config::Fleet::load(&bed.home_dirs.fleet()).unwrap();
        assert_eq!(f.hosts["twinoaks"].allow, None);
    }

    #[test]
    fn a_bad_allow_list_reprompts_instead_of_writing() {
        let bed = Bed::new("");
        let home = bed.home();
        pending_twinoaks(&bed, &home);
        let (r, out) = drive(&home, "y\n~/Downloads\n~/work/tundra\ny\n", None);
        r.unwrap();
        assert!(out.contains("not under a root"), "{out}");
        assert_eq!(out.matches("Allow list").count(), 2);
        let f = crate::config::Fleet::load(&bed.home_dirs.fleet()).unwrap();
        assert_eq!(
            f.hosts["twinoaks"].allow,
            Some(vec!["work/tundra".to_string()])
        );
    }

    #[test]
    fn declining_or_eof_writes_nothing() {
        let bed = Bed::new("");
        let home = bed.home();
        pending_twinoaks(&bed, &home);
        let before = std::fs::read_to_string(bed.home_dirs.fleet()).unwrap();
        for script in ["\n", "n\n", "y\n~/work\nn\n", "y\n", ""] {
            let (r, out) = drive(&home, script, None);
            r.unwrap();
            assert!(
                out.contains("Skipped") || out.contains("Not approved"),
                "{script:?}: {out}"
            );
            assert_eq!(
                std::fs::read_to_string(bed.home_dirs.fleet()).unwrap(),
                before,
                "{script:?}"
            );
        }
    }

    #[test]
    fn nothing_pending_and_unknown_host() {
        let bed = Bed::new("");
        let home = bed.home();
        let (r, out) = drive(&home, "", None);
        r.unwrap();
        assert_eq!(out, "Nothing pending.\n");
        pending_twinoaks(&bed, &home);
        let (r, _) = drive(&home, "", Some("ghost"));
        assert!(r.unwrap_err().to_string().contains("ghost is not pending"));
    }

    #[test]
    fn approve_mints_and_pins_and_leaves_the_bundle_for_collection() {
        let bed = Bed::new("");
        let home = bed.home();
        pending_twinoaks(&bed, &home);
        let (r, out) = drive(&home, "y\n\n\n", None);
        r.unwrap();
        let f = crate::config::Fleet::load(&bed.home_dirs.fleet()).unwrap();
        let pin = f.hosts["twinoaks"].cert.clone().expect("pinned");
        assert!(
            out.contains(&format!("Its certificate ({pin}) waits here")),
            "{out}"
        );
        let issued = bed.home_dirs.issued().join("twinoaks.pem");
        let id = crate::daemon::tls::Identity::load(&issued).unwrap();
        assert_eq!(
            id.fingerprint(),
            pin,
            "the waiting bundle is the pinned cert"
        );
        let mode = std::fs::metadata(&issued).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let dir = std::fs::metadata(bed.home_dirs.issued())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(dir & 0o777, 0o700);
        let log = home.log(&Default::default()).unwrap();
        let line = log.iter().find(|l| l.event == "approve").unwrap();
        assert!(line.detail.contains(&pin), "{}", line.detail);
    }

    /// The v0.1 upgrade and the lost-key case: `approve <host>` on an
    /// enrolled host re-mints and replaces the pin.
    #[test]
    fn approve_of_an_enrolled_host_reissues_after_asking() {
        let bed = Bed::enrolled();
        let home = bed.home();
        let before = std::fs::read_to_string(bed.home_dirs.fleet()).unwrap();
        for script in ["", "n\n", "\n"] {
            let (r, out) = drive(&home, script, Some("leaf"));
            r.unwrap();
            assert!(out.contains("Not reissued."), "{script:?}: {out}");
        }
        assert_eq!(
            std::fs::read_to_string(bed.home_dirs.fleet()).unwrap(),
            before
        );
        assert!(!bed.home_dirs.issued().join("leaf.pem").exists());

        let (r, out) = drive(&home, "y\n", Some("leaf"));
        r.unwrap();
        let first = crate::config::Fleet::load(&bed.home_dirs.fleet())
            .unwrap()
            .hosts["leaf"]
            .cert
            .clone()
            .expect("pinned");
        assert!(out.contains(&format!("new certificate ({first})")), "{out}");
        let (r, _) = drive(&home, "y\n", Some("leaf"));
        r.unwrap();
        let f = crate::config::Fleet::load(&bed.home_dirs.fleet()).unwrap();
        let second = f.hosts["leaf"].cert.clone().unwrap();
        assert_ne!(first, second, "the old pin is replaced");
        assert_eq!(
            f.subscription("leaf").paths,
            vec!["work"],
            "enrollment kept"
        );
        let id =
            crate::daemon::tls::Identity::load(&bed.home_dirs.issued().join("leaf.pem")).unwrap();
        assert_eq!(id.fingerprint(), second);
        let log = home.log(&Default::default()).unwrap();
        let last = log.iter().rfind(|l| l.event == "reissue").unwrap();
        assert!(
            last.detail.contains(&format!("replaces:{first}")),
            "{}",
            last.detail
        );
        assert!(
            home.reissue("ghost")
                .unwrap_err()
                .to_string()
                .contains("not enrolled")
        );
    }
}
