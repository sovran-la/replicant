//! The command line: argument parsing, path absolutizing, and rendering.
//! Everything else happens in the daemon; the CLI is a thin client of the
//! local Unix socket.

pub mod approve;
mod args;
pub mod render;
pub mod sync;

pub use args::{Cli, Cmd, ConfigCmd, HostCmd};

use crate::config::Dirs;
use crate::daemon::journal::Filter;
use crate::daemon::wire::{LocalReply, LocalRequest};
use crate::daemon::{self, certs, local};
use crate::{Error, Result, timefmt};
use std::io::IsTerminal;
use std::path::Path;

/// A path as typed in a shell: absolute, `~`-relative, or relative to cwd.
pub fn absolutize(raw: &str, cwd: &Path) -> String {
    if raw.starts_with('/') || raw.starts_with('~') {
        raw.to_string()
    } else {
        cwd.join(raw).display().to_string()
    }
}

/// `--confirm-delete`: a local path (starting with `/`, `~` or `.`),
/// absolutized, or else a unit's home-relative path as `status` prints it.
pub fn confirm_path(raw: &str, cwd: &Path) -> String {
    if raw.starts_with(['/', '~', '.']) {
        absolutize(raw, cwd)
    } else {
        raw.to_string()
    }
}

/// `--path` for `log`: strip `~/` or `$HOME/` so it compares with journal paths.
pub fn journal_path(raw: &str, home: &Path) -> String {
    let home = home.display().to_string();
    raw.strip_prefix("~/")
        .or_else(|| raw.strip_prefix(&format!("{home}/")))
        .unwrap_or(raw)
        .trim_end_matches('/')
        .to_string()
}

/// Run a parsed command; the return value is the process exit code.
pub fn run(cli: Cli, dirs: &Dirs) -> Result<i32> {
    let style = render::Style {
        color: std::io::stdout().is_terminal(),
    };
    let call = |req| local::call(dirs, req);
    let cwd = std::env::current_dir()?;
    let json = |v: &dyn erased::Json| -> Result<i32> {
        println!("{}", v.json()?);
        Ok(0)
    };
    match cli.cmd {
        Cmd::Daemon => {
            let d = daemon::Daemon::start(dirs.clone())?;
            let role = if dirs.is_home() { "home" } else { "leaf" };
            eprintln!(
                "replicant {} daemon ({role}) at {}",
                env!("CARGO_PKG_VERSION"),
                dirs.socket().display()
            );
            if let Some(a) = d.tcp_addr() {
                eprintln!("listening for leaves on mtls://{a}");
            }
            d.wait();
            Ok(0)
        }
        Cmd::Subscribe {
            home,
            paths,
            as_local,
            address,
            credentials,
        } => {
            let paths = paths.iter().map(|p| absolutize(p, &cwd)).collect();
            let as_local = as_local.map(|p| if p.is_absolute() { p } else { cwd.join(p) });
            let req = LocalRequest::Subscribe {
                home,
                address,
                credentials,
                paths,
                as_local,
            };
            let LocalReply::Subscribed(o) = call(req)? else {
                return unexpected();
            };
            print!("{}", render::subscribed(&o));
            Ok(0)
        }
        Cmd::Unsubscribe { paths } => {
            let paths = paths.iter().map(|p| absolutize(p, &cwd)).collect();
            let LocalReply::Unsubscribed(o) = call(LocalRequest::Unsubscribe { paths })? else {
                return unexpected();
            };
            print!("{}", render::unsubscribed(&o));
            Ok(0)
        }
        Cmd::Approve { host } => {
            let stdin = std::io::stdin();
            approve::run(
                &mut stdin.lock(),
                &mut std::io::stdout(),
                host.as_deref(),
                &call,
            )?;
            Ok(0)
        }
        Cmd::Status {
            fleet,
            json: as_json,
        } => {
            let reply = call(if fleet {
                LocalRequest::Fleet
            } else {
                LocalRequest::Status
            })?;
            if as_json {
                return json(&reply);
            }
            match reply {
                LocalReply::Fleet(f) => print!("{}", render::fleet(&f, style)),
                LocalReply::LeafStatus(s) => print!("{}", render::leaf_status(&s, style)),
                _ => return unexpected(),
            }
            Ok(0)
        }
        Cmd::Sync {
            dry_run,
            yes,
            non_interactive,
            confirm_delete,
            accept_rollback,
            no_wait,
            json,
            verbose,
        } => {
            let a = sync::Args {
                dry_run,
                yes,
                interactive: !non_interactive && std::io::stdin().is_terminal(),
                confirm_delete: confirm_delete
                    .iter()
                    .map(|p| confirm_path(p, &cwd))
                    .collect(),
                accept_rollback: accept_rollback
                    .iter()
                    .map(|p| confirm_path(p, &cwd))
                    .collect(),
                no_wait,
                json,
                verbose,
                home_dir: crate::config::user_home()?,
            };
            let (stdin, mut stdout, mut stderr) =
                (std::io::stdin(), std::io::stdout(), std::io::stderr());
            let mut input = stdin.lock();
            let mut t = sync::Term {
                input: &mut input,
                out: &mut stdout,
                err: &mut stderr,
                style,
            };
            sync::run(&a, &mut t, &|req, on| local::call_streaming(dirs, req, on))
        }
        Cmd::Rollback { push } => match call(LocalRequest::Rollback { push })? {
            LocalReply::RolledBack(r) => {
                print!("{}", render::rolled_back(&r));
                Ok(0)
            }
            LocalReply::Pushes { pushes } => {
                print!("{}", render::pushes(&pushes, crate::now_secs()));
                Ok(0)
            }
            _ => unexpected(),
        },
        Cmd::Log { host, path, since } => {
            let since = match since {
                Some(s) => Some(timefmt::parse_since(&s, crate::now_secs()).ok_or_else(|| {
                    Error::Config(format!(
                        "--since {s:?}: use 2026-10-31, 2026-10-31T12:00:00Z, or 90m/2h/3d"
                    ))
                })?),
                None => None,
            };
            let home = crate::config::user_home()?;
            let filter = Filter {
                host,
                path: path.map(|p| journal_path(&p, &home)),
                since,
            };
            let LocalReply::Log { lines } = call(LocalRequest::Log { filter })? else {
                return unexpected();
            };
            print!("{}", render::log(&lines));
            Ok(0)
        }
        Cmd::Config {
            cmd: ConfigCmd::ExportCreds,
        } => {
            if !dirs.is_home() {
                return Err(Error::Refused(
                    "export-creds runs on home (the machine with fleet.toml)".into(),
                ));
            }
            certs::ensure(&certs::Paths::of(dirs))?;
            print!("{}", certs::read_pem(&dirs.credentials())?);
            Ok(0)
        }
        Cmd::Host {
            cmd: HostCmd::Revoke { host },
        } => {
            let LocalReply::Revoked(r) = call(LocalRequest::Revoke { host })? else {
                return unexpected();
            };
            print!("{}", render::revoked(&r));
            Ok(0)
        }
        Cmd::Config {
            cmd: ConfigCmd::Show,
        } => {
            println!("directory: {}", dirs.root.display());
            println!(
                "role:      {}",
                if dirs.is_home() {
                    "home (fleet.toml present)"
                } else {
                    "leaf"
                }
            );
            println!("socket:    {}", dirs.socket().display());
            println!("journal:   {}", dirs.journal().display());
            Ok(0)
        }
    }
}

fn unexpected() -> Result<i32> {
    Err(unexpected_reply())
}

pub(crate) fn unexpected_reply() -> Error {
    Error::Daemon("the daemon sent an unexpected reply; is it the same version as this CLI?".into())
}

/// Pretty JSON for `--json`, over any serializable reply.
mod erased {
    pub trait Json {
        fn json(&self) -> crate::Result<String>;
    }
    impl<T: serde::Serialize> Json for T {
        fn json(&self) -> crate::Result<String> {
            serde_json::to_string_pretty(self).map_err(|e| crate::Error::Daemon(e.to_string()))
        }
    }
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
