use clap::Parser;
use replicant::cli::{Cli, run};
use replicant::config::Dirs;

fn main() {
    let cli = Cli::parse();
    let code = match Dirs::discover().and_then(|dirs| run(cli, &dirs)) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("replicant: {e}");
            1
        }
    };
    std::process::exit(code);
}
