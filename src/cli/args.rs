//! The command line as clap sees it. PLAN.md, "Implementation sketch":
//! subscribe | unsubscribe | approve | sync | status | rollback | log |
//! daemon | config | host.
//! (`hold` and `release` arrive with the phase that needs them.)

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "replicant",
    version,
    about = "Home-base-mediated sync for ~/work"
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
        /// Home's daemon address, host:port. Repeat it to list several, in
        /// the order to try: the LAN name first, the tailnet address after.
        #[arg(long, action = clap::ArgAction::Append)]
        address: Vec<String>,
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
    /// The decision table and open markers, without acting (on home: the
    /// whole fleet).
    Status {
        #[arg(long)]
        fleet: bool,
        #[arg(long)]
        json: bool,
    },
    /// Decide each unit's direction, show it, and carry it out.
    ///
    /// On a terminal the plan is shown and `y` asked for (each deletion
    /// first, by itself); --yes skips the question. Nothing is written
    /// while anything needs a human: a conflict, a quarantine, a guardrail
    /// refusal, an unconfirmed deletion (exit 2). A unit whose transfer does
    /// not finish stops the sync there; its marker stays and the next sync
    /// resumes it (exit 1). What a push replaces on home is kept as its
    /// backup.
    Sync {
        /// Show the decision and each unit's itemized diff; write nothing.
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
        /// its home-relative path as `status` prints it (work/r). Also lets
        /// an interrupted sync's resume replace or delete what changed here
        /// since it was interrupted (guardrail 6). Repeatable. Required
        /// even with --yes.
        #[arg(long = "confirm-delete", value_name = "PATH")]
        confirm_delete: Vec<String>,
        /// Take home's copy of a unit whose push from here home rolled back
        /// (`replicant rollback`): until then sync holds it as a conflict
        /// rather than pull over what this machine pushed. Named like
        /// --confirm-delete. Repeatable. Required even with --yes.
        #[arg(long = "accept-rollback", value_name = "PATH")]
        accept_rollback: Vec<String>,
        /// Fail instead of waiting behind another sync.
        #[arg(long)]
        no_wait: bool,
        /// Print the plan (--dry-run) or the report (--yes) as JSON.
        #[arg(long)]
        json: bool,
        /// Show every line of each unit's diff.
        #[arg(short, long)]
        verbose: bool,
    },
    /// On home: undo one push, or list the pushes.
    ///
    /// Files the push replaced or deleted come back from its backup; files
    /// it created are removed; what it wrote is kept in the push's backup
    /// directory first. Its marker (a quarantine) is cleared; baselines are
    /// not touched. The pushing leaf's next sync holds the unit as a
    /// conflict until it runs `sync --accept-rollback <path>`, which pulls
    /// the result.
    Rollback {
        /// `<host>/<YYYYMMDDTHHMMSSZ>`, as `sync` and `log` print it
        /// (`backup:...`). Without it, every push home still has a backup of.
        #[arg(value_name = "PUSH-ID")]
        push: Option<String>,
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
