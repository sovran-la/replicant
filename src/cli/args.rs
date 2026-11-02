//! The command line as clap sees it. PLAN.md, "Implementation sketch":
//! subscribe | unsubscribe | approve | sync | status | log | daemon | config
//! | host.
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
    /// Decide the direction per unit, show it, and (after `y`, or with
    /// --yes) carry it out: pull, push, remove. Nothing is written while
    /// anything needs a human (exit 2); an rsync that does not finish
    /// stops the sync and the next one resumes it (exit 1).
    Sync {
        /// Show the decision and the itemized rsync diff; write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Apply without asking. Does not confirm deletions.
        #[arg(long)]
        yes: bool,
        /// Never prompt (as without a terminal): with --yes it applies,
        /// without it it only shows the plan. Reports go to stderr.
        #[arg(long)]
        non_interactive: bool,
        /// Carry out the deletion of a unit that vanished on one side since
        /// it was last synced: its path here (starting with /, ~ or .) or
        /// its home-relative path as `status` prints it (work/r).
        /// Repeatable. Required even with --yes.
        #[arg(long = "confirm-delete", value_name = "PATH")]
        confirm_delete: Vec<String>,
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
    /// On home: manage an enrolled host.
    Host {
        #[command(subcommand)]
        cmd: HostCmd,
    },
}

#[derive(Debug, Subcommand)]
pub enum HostCmd {
    /// Remove the host's certificate pin: its requests are refused until
    /// `replicant approve <host>` issues a new one.
    Revoke { host: String },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    /// On home: print the bootstrap bundle a new leaf saves as home.pem.
    ExportCreds,
    /// Where replicant keeps things and which role this machine has.
    Show,
}
