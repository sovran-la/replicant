//! Argument parsing and the path helpers.

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
    let c = Cli::try_parse_from(["replicant", "host", "revoke", "birchbook"]).unwrap();
    assert!(matches!(
        c.cmd,
        Cmd::Host { cmd: HostCmd::Revoke { ref host } } if host == "birchbook"
    ));
    assert!(Cli::try_parse_from(["replicant", "host", "revoke"]).is_err());
    let c = Cli::try_parse_from(["replicant", "status", "--fleet"]).unwrap();
    assert!(matches!(c.cmd, Cmd::Status { fleet: true, .. }));
    assert!(Cli::try_parse_from(["replicant", "log", "--since", "2h", "--host", "x"]).is_ok());
}

#[test]
fn absolutize_and_journal_path() {
    let cwd = Path::new("/Users/b/work");
    assert_eq!(absolutize("tundra", cwd), "/Users/b/work/tundra");
    assert_eq!(absolutize("~/work", cwd), "~/work");
    assert_eq!(absolutize("/abs", cwd), "/abs");
    assert_eq!(confirm_path("work/r", cwd), "work/r", "a unit path");
    assert_eq!(confirm_path("./r", cwd), "/Users/b/work/./r");
    assert_eq!(confirm_path("~/work2/r", cwd), "~/work2/r");
    assert_eq!(confirm_path("/x/r", cwd), "/x/r");
    let home = Path::new("/Users/b");
    assert_eq!(journal_path("~/work/x/", home), "work/x");
    assert_eq!(journal_path("/Users/b/work/x", home), "work/x");
    assert_eq!(journal_path("work/x", home), "work/x");
}
