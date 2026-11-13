use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "bazel-kotlin-lsp",
    version,
    about = "Kotlin navigation for Bazel projects through kotlin-lsp"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the Bazel aspect and write kotlin-lsp's workspace.json.
    Sync(SyncArgs),
}

#[derive(Debug, Args)]
pub struct SyncArgs {
    /// Folder inside the Bazel workspace (default: current directory).
    #[arg(long, value_name = "DIR")]
    pub workspace: Option<PathBuf>,
    /// Folder for workspace.json and logs (default: the cache folder).
    #[arg(long, value_name = "DIR")]
    pub out: Option<PathBuf>,
    /// Bazel binary (default: bazelisk, then bazel, on PATH).
    #[arg(long, value_name = "PATH")]
    pub bazel: Option<PathBuf>,
    /// Directory to model as source modules, relative to the workspace root; a leading `-` excludes it.
    #[arg(long, value_name = "DIR", allow_hyphen_values = true)]
    pub scope_dir: Vec<String>,
    /// Extra target to model as a source module.
    #[arg(long, value_name = "LABEL")]
    pub target: Vec<String>,
    /// Use the user's own Bazel output base instead of a dedicated one.
    #[arg(long)]
    pub shared_output_base: bool,
    /// Cache root instead of the platform cache folder (tests and benchmarks).
    #[arg(long, value_name = "DIR", hide = true)]
    pub cache_root: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn sync_parses_repeatable_scope_flags() {
        let cli = Cli::try_parse_from([
            "bazel-kotlin-lsp",
            "sync",
            "--scope-dir",
            "app",
            "--scope-dir",
            "-app/legacy",
            "--target",
            "//tools:gen",
        ])
        .unwrap();
        let Command::Sync(a) = cli.command;
        assert_eq!(a.scope_dir, ["app", "-app/legacy"]);
        assert_eq!(a.target, ["//tools:gen"]);
        assert!(!a.shared_output_base);
    }

    #[test]
    fn hidden_cache_root_flag_parses() {
        let cli =
            Cli::try_parse_from(["bazel-kotlin-lsp", "sync", "--cache-root", "/tmp/c"]).unwrap();
        let Command::Sync(a) = cli.command;
        assert_eq!(a.cache_root, Some(PathBuf::from("/tmp/c")));
    }

    #[test]
    fn unknown_flag_is_a_usage_error() {
        assert!(Cli::try_parse_from(["bazel-kotlin-lsp", "sync", "--nope"]).is_err());
    }
}
