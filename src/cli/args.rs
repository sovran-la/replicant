//! The command line as clap sees it. PLAN.md, "Implementation sketch":
//! subscribe | unsubscribe | approve | sync | status | log | daemon | config.
//! (`hold`, `release`, `rollback` arrive with the phases that need them.)

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "replicant",
    version,
    about = "Home-base-mediated rsync for ~/work"
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Run this machine's daemon (home if fleet.toml exists, else leaf).
    Daemon,
    /// Ask home to serve paths to this machine.
    Subscribe {
        /// Home's name (and, unless --address says otherwise, its address).
        home: String,
        #[arg(required = true)]
        paths: Vec<String>,
        /// Map the (single) path to a different local path.
        #[arg(long = "as")]
        as_local: Option<PathBuf>,
        /// Home's daemon address, host:port.
        #[arg(long)]
        address: Option<String>,
        /// The ssh destination rsync uses for home ("local" for same machine).
        #[arg(long)]
        rsync_host: Option<String>,
        /// Home's bundle, if not ~/.sovran/replicant/home.pem.
        #[arg(long)]
        credentials: Option<String>,
    },
    /// Stop receiving paths; the next sync removes them here.
    Unsubscribe {
        #[arg(required = true)]
        paths: Vec<String>,
    },
    /// On home: review pending hosts and decide their caps.
    Approve { host: Option<String> },
    /// The decision table without acting (on home: the whole fleet).
    Status {
        #[arg(long)]
        fleet: bool,
        #[arg(long)]
        json: bool,
    },
    /// Decide the direction and show it. v0.1 supports --dry-run only.
    Sync {
        #[arg(long)]
        dry_run: bool,
        /// Fail instead of waiting behind another sync.
        #[arg(long)]
        no_wait: bool,
        #[arg(long)]
        json: bool,
        #[arg(short, long)]
        verbose: bool,
    },
    /// Read the journal.
    Log {
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        path: Option<String>,
        /// ISO time, a date, or an age like 2h or 3d.
        #[arg(long)]
        since: Option<String>,
    },
    /// Local configuration.
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    /// On home: print the bootstrap bundle a new leaf saves as home.pem.
    ExportCreds,
    /// Where replicant keeps things and which role this machine has.
    Show,
}
