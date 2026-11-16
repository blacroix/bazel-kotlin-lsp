use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::sync::SyncOptions;

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
    /// Run kotlin-lsp behind an LSP proxy that keeps workspace.json in sync with Bazel.
    Proxy(ProxyArgs),
    /// Remove the caches (Bazel output base, model, kotlin-lsp index) of workspaces that no longer exist.
    Gc(GcArgs),
}

#[derive(Debug, Args)]
pub struct GcArgs {
    /// List what would be removed, and remove nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Remove this workspace's cache now, even if the workspace still exists; nothing else.
    #[arg(long, value_name = "DIR")]
    pub workspace: Option<PathBuf>,
    /// Cache root instead of the platform cache folder (tests).
    #[arg(long, value_name = "DIR", hide = true)]
    pub cache_root: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ProxyArgs {
    #[command(flatten)]
    pub sync: SyncArgs,
    /// The kotlin-lsp command and its arguments, after `--`.
    #[arg(last = true, required = true, value_name = "KOTLIN_LSP_COMMAND")]
    pub command: Vec<String>,
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
    /// Directory to model as source modules, relative to the workspace root (or absolute inside it); a leading `-`
    /// excludes it.
    #[arg(long, value_name = "DIR", allow_hyphen_values = true)]
    pub scope_dir: Vec<String>,
    /// Extra target to model as a source module, in addition to the directories (the whole repo if none).
    #[arg(long, value_name = "LABEL")]
    pub target: Vec<String>,
    /// Use the user's own Bazel output base instead of a dedicated one.
    #[arg(long)]
    pub shared_output_base: bool,
    /// Cache root instead of the platform cache folder (tests and benchmarks).
    #[arg(long, value_name = "DIR", hide = true)]
    pub cache_root: Option<PathBuf>,
}

impl SyncArgs {
    pub fn into_options(self, echo_bazel: bool) -> SyncOptions {
        SyncOptions {
            start_dir: self.workspace.unwrap_or_else(|| {
                default_start_dir(
                    std::env::var_os("PWD"),
                    std::env::current_dir().unwrap_or_default(),
                )
            }),
            out_dir: self.out,
            bazel: self.bazel,
            scope_dirs: self.scope_dir,
            targets: self.target,
            shared_output_base: self.shared_output_base,
            cache_root: self.cache_root,
            echo_bazel,
            cancel: None,
            progress: None,
        }
    }
}

/// The folder `sync` starts from without `--workspace`: the shell's logical `$PWD` (which keeps a symlinked path)
/// when it is the current directory, else the current directory.
pub fn default_start_dir(pwd: Option<std::ffi::OsString>, cwd: PathBuf) -> PathBuf {
    let same = |p: &PathBuf| {
        p.canonicalize()
            .ok()
            .zip(cwd.canonicalize().ok())
            .is_some_and(|(a, b)| a == b)
    };
    pwd.map(PathBuf::from)
        .filter(|p| p.is_absolute() && same(p))
        .unwrap_or(cwd)
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
        let Command::Sync(a) = cli.command else {
            panic!("not sync")
        };
        assert_eq!(a.scope_dir, ["app", "-app/legacy"]);
        assert_eq!(a.target, ["//tools:gen"]);
        assert!(!a.shared_output_base);
    }

    #[test]
    fn hidden_cache_root_flag_parses() {
        let cli =
            Cli::try_parse_from(["bazel-kotlin-lsp", "sync", "--cache-root", "/tmp/c"]).unwrap();
        let Command::Sync(a) = cli.command else {
            panic!("not sync")
        };
        assert_eq!(a.cache_root, Some(PathBuf::from("/tmp/c")));
    }

    #[test]
    fn default_start_dir_prefers_logical_pwd() {
        let t = tempfile::tempdir().unwrap();
        let real = t.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = t.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(
            default_start_dir(Some(link.clone().into()), real.clone()),
            link
        );
        assert_eq!(default_start_dir(Some(t.path().into()), real.clone()), real);
        assert_eq!(default_start_dir(None, real.clone()), real);
    }

    #[test]
    fn proxy_takes_sync_flags_and_the_command_after_dashes() {
        let cli = Cli::try_parse_from([
            "bazel-kotlin-lsp",
            "proxy",
            "--workspace",
            "/w",
            "--scope-dir",
            "app",
            "--",
            "/k/intellij-server",
            "--stdio",
        ])
        .unwrap();
        let Command::Proxy(p) = cli.command else {
            panic!("not proxy")
        };
        assert_eq!(p.sync.workspace, Some(PathBuf::from("/w")));
        assert_eq!(p.sync.scope_dir, ["app"]);
        assert_eq!(p.command, ["/k/intellij-server", "--stdio"]);
        assert!(Cli::try_parse_from(["bazel-kotlin-lsp", "proxy"]).is_err());
    }

    #[test]
    fn unknown_flag_is_a_usage_error() {
        assert!(Cli::try_parse_from(["bazel-kotlin-lsp", "sync", "--nope"]).is_err());
    }
}
