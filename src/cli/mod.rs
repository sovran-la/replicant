//! The command line: argument parsing, path absolutizing, and rendering.
//! Everything else happens in the daemon; the CLI is a thin client of the
//! local Unix socket.

pub mod approve;
mod args;
pub mod render;

pub use args::{Cli, Cmd, ConfigCmd};

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
            rsync_host,
            credentials,
        } => {
            let paths = paths.iter().map(|p| absolutize(p, &cwd)).collect();
            let as_local = as_local.map(|p| if p.is_absolute() { p } else { cwd.join(p) });
            let req = LocalRequest::Subscribe {
                home,
                address,
                rsync_host,
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
            no_wait,
            json: as_json,
            verbose,
        } => {
            if !dry_run {
                return Err(Error::Refused(
                    "replicant 0.1 is read-only: only `sync --dry-run` exists; apply lands in 0.2"
                        .into(),
                ));
            }
            let LocalReply::Plan(p) = call(LocalRequest::DryRun { no_wait })? else {
                return unexpected();
            };
            if as_json {
                json(&*p)?;
            } else {
                print!("{}", render::plan(&p, verbose, style));
            }
            Ok(if p.blocked() { 2 } else { 0 })
        }
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
            certs::ensure(&dirs.ca_key(), &dirs.ca_cert(), &dirs.credentials())?;
            print!("{}", certs::read_pem(&dirs.credentials())?);
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
    Err(Error::Daemon(
        "the daemon sent an unexpected reply; is it the same version as this CLI?".into(),
    ))
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
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_the_plan_examples() {
        let c = Cli::try_parse_from([
            "replicant",
            "subscribe",
            "thistledown",
            "/Users/b/work/tundra",
            "--as",
            "/Users/b/work2/tundra",
        ])
        .unwrap();
        assert!(matches!(c.cmd, Cmd::Subscribe { ref as_local, .. } if as_local.is_some()));
        assert!(
            Cli::try_parse_from(["replicant", "subscribe", "thistledown"]).is_err(),
            "paths required"
        );
        let c = Cli::try_parse_from(["replicant", "sync", "--dry-run", "--no-wait"]).unwrap();
        assert!(matches!(
            c.cmd,
            Cmd::Sync {
                dry_run: true,
                no_wait: true,
                ..
            }
        ));
        let c = Cli::try_parse_from(["replicant", "status", "--fleet"]).unwrap();
        assert!(matches!(c.cmd, Cmd::Status { fleet: true, .. }));
        assert!(Cli::try_parse_from(["replicant", "log", "--since", "2h", "--host", "x"]).is_ok());
    }

    #[test]
    fn sync_without_dry_run_is_refused_before_touching_anything() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = Dirs::at(tmp.path().to_path_buf());
        let c = Cli::try_parse_from(["replicant", "sync"]).unwrap();
        let err = run(c, &dirs).unwrap_err().to_string();
        assert!(err.contains("read-only"), "{err}");
    }

    #[test]
    fn absolutize_and_journal_path() {
        let cwd = Path::new("/Users/b/work");
        assert_eq!(absolutize("tundra", cwd), "/Users/b/work/tundra");
        assert_eq!(absolutize("~/work", cwd), "~/work");
        assert_eq!(absolutize("/abs", cwd), "/abs");
        let home = Path::new("/Users/b");
        assert_eq!(journal_path("~/work/x/", home), "work/x");
        assert_eq!(journal_path("/Users/b/work/x", home), "work/x");
        assert_eq!(journal_path("work/x", home), "work/x");
    }
}
