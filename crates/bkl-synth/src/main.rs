use std::path::PathBuf;

use bkl_synth::bench::{BenchArgs, bench, markdown};
use bkl_synth::generate::{Spec, generate};
use clap::{Parser, Subcommand};

/// Synthetic Bazel repositories for benchmarking bazel-kotlin-lsp.
#[derive(Parser)]
#[command(name = "bkl-synth")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Write a synthetic repository.
    Generate {
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 20)]
        modules: usize,
        #[arg(long, default_value_t = 100)]
        targets_per_module: usize,
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
    /// Time sync on a generated repository (it is modified: a package is added).
    Bench {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        sync_bin: PathBuf,
        #[arg(long)]
        kotlin_lsp: Option<PathBuf>,
        #[arg(long, default_value = "bench.json")]
        out: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Generate {
            out,
            modules,
            targets_per_module,
            seed,
        } => {
            generate(
                &Spec {
                    modules,
                    targets_per_module,
                    seed,
                },
                &out,
            )?;
            println!(
                "{} targets in {}",
                modules * targets_per_module + 1,
                out.display()
            );
        }
        Command::Bench {
            repo,
            sync_bin,
            kotlin_lsp,
            out,
        } => {
            let report = bench(&BenchArgs {
                repo,
                sync_bin,
                kotlin_lsp,
                out,
            })?;
            print!("{}", markdown(&report));
        }
    }
    Ok(())
}
