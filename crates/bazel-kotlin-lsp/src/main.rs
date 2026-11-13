use std::process::ExitCode;

use bazel_kotlin_lsp::cli::{Cli, Command};
use clap::Parser;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Sync(_) => {
            eprintln!("sync: not implemented");
            ExitCode::from(1)
        }
    }
}
