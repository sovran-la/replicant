use clap::Parser;
use replicant::cli::{Cli, run};
use replicant::config::Dirs;

fn main() {
    // Die quietly on a closed pipe (`replicant log | head`) like any Unix
    // tool, instead of panicking inside println!.
    // SAFETY: restoring the default disposition of one signal at startup,
    // before any thread exists.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
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
