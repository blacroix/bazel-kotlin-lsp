use std::process::ExitCode;

use bazel_kotlin_lsp::cli::{Cli, Command};
use bazel_kotlin_lsp::gc::{self, GcOptions};
use bazel_kotlin_lsp::proxy::{self, ProxyOptions};
use bazel_kotlin_lsp::sync::{now_secs, platform_cache_root, run_sync};
use clap::Parser;

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Sync(args) => match run_sync(&args.into_options(true)) {
            Ok(outcome) => {
                let s = &outcome.summary;
                for warning in &s.warnings {
                    eprintln!("warning: {warning}");
                }
                let change = if s.changed { "updated" } else { "unchanged" };
                eprintln!(
                    "{} modules, {} libraries ({}); workspace.json {change} ({})",
                    s.modules, s.libraries, s.scope, s.status
                );
                println!("{}", outcome.model_dir.display());
                ExitCode::from(if s.status == "ok" { 0 } else { 3 })
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(1)
            }
        },
        Command::Proxy(args) => {
            let code = proxy::run(ProxyOptions {
                sync: args.sync.into_options(false),
                command: args.command,
                zed_server: args.zed_server,
            });
            ExitCode::from(u8::try_from(code).unwrap_or(1))
        }
        Command::Gc(args) => {
            let Some(cache_root) = args.cache_root.or_else(platform_cache_root) else {
                eprintln!("error: cannot find a cache folder: set HOME to an absolute path");
                return ExitCode::from(1);
            };
            let opts = GcOptions {
                cache_root,
                dry_run: args.dry_run,
                workspace: args.workspace,
            };
            match gc::gc(&opts, now_secs()) {
                Ok(entries) => {
                    print!("{}", gc::report(&entries));
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {e:#}");
                    ExitCode::from(1)
                }
            }
        }
    }
}
