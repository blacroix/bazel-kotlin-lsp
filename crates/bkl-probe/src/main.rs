use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use bkl_probe::{ProbeArgs, run};
use clap::Parser;

/// Headless LSP probe: go-to-definition (and diagnostics) on known symbols, PASS/FAIL per case.
#[derive(Parser)]
#[command(name = "bkl-probe")]
struct Args {
    #[arg(long)]
    root: PathBuf,
    #[arg(long)]
    cases: PathBuf,
    /// Folder holding workspace.json.
    #[arg(long)]
    model_dir: PathBuf,
    #[arg(long)]
    index_dir: Option<PathBuf>,
    /// Seconds each case may take.
    #[arg(long, default_value_t = 180)]
    timeout: u64,
    /// File for the server's stderr.
    #[arg(long)]
    log: Option<PathBuf>,
    /// The language server command, after `--`.
    #[arg(last = true, required = true)]
    server: Vec<String>,
}

fn main() -> ExitCode {
    let a = Args::parse();
    let args = ProbeArgs {
        root: a.root,
        cases: a.cases,
        model_dir: a.model_dir,
        index_dir: a.index_dir,
        timeout: Duration::from_secs(a.timeout),
        log: a.log,
        server: a.server,
    };
    match run(&args) {
        Ok(results) => {
            for r in &results {
                println!(
                    "{}  {}\n      -> {:?}",
                    if r.pass { "PASS" } else { "FAIL" },
                    r.name,
                    r.targets
                );
            }
            let passed = results.iter().filter(|r| r.pass).count();
            println!("# {passed}/{} passed", results.len());
            ExitCode::from(if passed == results.len() { 0 } else { 1 })
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}
