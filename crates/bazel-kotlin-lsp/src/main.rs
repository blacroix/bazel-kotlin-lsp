use std::process::ExitCode;

use bazel_kotlin_lsp::cli::{Cli, Command, default_start_dir};
use bazel_kotlin_lsp::sync::{SyncOptions, run_sync};
use clap::Parser;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Sync(args) => {
            let options = SyncOptions {
                start_dir: args.workspace.unwrap_or_else(|| {
                    default_start_dir(
                        std::env::var_os("PWD"),
                        std::env::current_dir().unwrap_or_default(),
                    )
                }),
                out_dir: args.out,
                bazel: args.bazel,
                scope_dirs: args.scope_dir,
                targets: args.target,
                shared_output_base: args.shared_output_base,
                cache_root: args.cache_root,
                echo_bazel: true,
            };
            match run_sync(&options) {
                Ok(outcome) => {
                    let s = &outcome.summary;
                    for warning in &s.warnings {
                        eprintln!("warning: {warning}");
                    }
                    let change = if s.changed { "updated" } else { "unchanged" };
                    eprintln!(
                        "{} modules, {} libraries; workspace.json {change} ({})",
                        s.modules, s.libraries, s.status
                    );
                    println!("{}", outcome.model_dir.display());
                    ExitCode::from(if s.status == "ok" { 0 } else { 3 })
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::from(1)
                }
            }
        }
    }
}
