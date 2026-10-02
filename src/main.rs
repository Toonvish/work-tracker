use std::process::ExitCode;

use clap::Parser;
use work_tracker::cli::Cli;

fn main() -> ExitCode {
    match work_tracker::app::run(Cli::parse()) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("error: {:#}", e.err);
            ExitCode::from(e.code)
        }
    }
}
