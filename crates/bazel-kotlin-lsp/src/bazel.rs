//! Running Bazel with the flags every sync invocation shares.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, bail};
use bkl_core::aspect_template::{ASPECT, ASPECT_REPO};
use bkl_core::version::{BazelVersion, parse_bazel_version};

const STDERR_TAIL: usize = 20;

#[derive(Debug, Clone)]
pub struct Bazel {
    pub binary: PathBuf,
    pub workspace: PathBuf,
    /// A dedicated output base; `None` shares the user's.
    pub output_base: Option<PathBuf>,
    pub aspect_dir: PathBuf,
    pub log: PathBuf,
    /// Also copy Bazel's stderr to ours (for the CLI).
    pub echo: bool,
}

#[derive(Debug, Clone)]
pub struct BazelRun {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr_tail: Vec<String>,
    pub command_line: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildOutcome {
    Full,
    Partial,
    Failed,
}

/// No aspect output is always a failure (the stdlib target alone produces some). Otherwise 0 is a full build, 1
/// (build or analysis failure under `--keep_going`) is partial, and anything else (bad flags, crash,
/// interrupted) is a failure.
pub fn classify_build(exit_code: i32, json_files: usize) -> BuildOutcome {
    match (exit_code, json_files) {
        (_, 0) => BuildOutcome::Failed,
        (0, _) => BuildOutcome::Full,
        (1, _) => BuildOutcome::Partial,
        _ => BuildOutcome::Failed,
    }
}

/// The explicit binary, else `bazelisk`, else `bazel`, on `PATH`.
pub fn resolve_binary(explicit: Option<&Path>) -> anyhow::Result<PathBuf> {
    resolve_binary_in(explicit, std::env::var_os("PATH"))
}

pub(crate) fn resolve_binary_in(
    explicit: Option<&Path>,
    path_var: Option<OsString>,
) -> anyhow::Result<PathBuf> {
    if let Some(explicit) = explicit {
        return Ok(explicit.to_path_buf());
    }
    let dirs: Vec<PathBuf> = std::env::split_paths(&path_var.unwrap_or_default()).collect();
    for name in ["bazelisk", "bazel"] {
        if let Some(found) = dirs.iter().map(|d| d.join(name)).find(|p| p.is_file()) {
            return Ok(found);
        }
    }
    bail!("neither bazelisk nor bazel is on PATH; install one or pass --bazel <path>")
}

impl Bazel {
    pub fn argv(&self, command: &str, args: &[String]) -> Vec<String> {
        let mut argv = Vec::new();
        if let Some(output_base) = &self.output_base {
            argv.push(format!("--output_base={}", output_base.display()));
        }
        argv.push(command.to_string());
        argv.push("--lockfile_mode=off".into());
        argv.push(format!(
            "--inject_repository={ASPECT_REPO}={}",
            self.aspect_dir.display()
        ));
        argv.push("--color=no".into());
        argv.push("--curses=no".into());
        argv.extend(args.iter().cloned());
        argv
    }

    pub fn build_args(patterns_file: &Path, output_group: &str, bep: Option<&Path>) -> Vec<String> {
        let mut args = vec![
            "--symlink_prefix=/".to_string(),
            "--keep_going".into(),
            format!("--aspects={ASPECT}"),
            format!("--output_groups={output_group}"),
            "--remote_download_outputs=toplevel".into(),
            format!("--target_pattern_file={}", patterns_file.display()),
        ];
        if let Some(bep) = bep {
            args.push(format!("--build_event_json_file={}", bep.display()));
        }
        args
    }

    pub fn version(&self) -> anyhow::Result<Option<BazelVersion>> {
        let out = Command::new(&self.binary)
            .arg("--version")
            .current_dir(&self.workspace)
            .output()
            .with_context(|| format!("running {} --version", self.binary.display()))?;
        if !out.status.success() {
            bail!(
                "{} --version failed: {}",
                self.binary.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(parse_bazel_version(&String::from_utf8_lossy(&out.stdout)))
    }

    /// Runs one Bazel command in the workspace, appending the command line and all of stderr to the log.
    pub fn run(&self, command: &str, args: &[String]) -> anyhow::Result<BazelRun> {
        let argv = self.argv(command, args);
        let command_line = shlex::try_join(
            std::iter::once(self.binary.to_string_lossy().as_ref())
                .chain(argv.iter().map(String::as_str)),
        )
        .unwrap_or_else(|_| format!("{} {}", self.binary.display(), argv.join(" ")));
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .with_context(|| format!("opening {}", self.log.display()))?;
        writeln!(log, "$ {command_line}")?;

        let mut child = Command::new(&self.binary)
            .args(&argv)
            .current_dir(&self.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("starting {}", self.binary.display()))?;
        let mut stdout = child.stdout.take().expect("piped");
        let reader = std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = stdout.read_to_string(&mut buf);
            buf
        });
        let mut tail = VecDeque::with_capacity(STDERR_TAIL);
        for line in BufReader::new(child.stderr.take().expect("piped")).lines() {
            let line = line.unwrap_or_default();
            writeln!(log, "{line}")?;
            if self.echo {
                eprintln!("{line}");
            }
            if tail.len() == STDERR_TAIL {
                tail.pop_front();
            }
            tail.push_back(line);
        }
        let status = child.wait()?;
        let stdout = reader.join().unwrap_or_default();
        Ok(BazelRun {
            exit_code: status.code().unwrap_or(-1),
            stdout,
            stderr_tail: tail.into(),
            command_line,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bazel(output_base: Option<&str>) -> Bazel {
        Bazel {
            binary: "/bin/bazelisk".into(),
            workspace: "/ws".into(),
            output_base: output_base.map(PathBuf::from),
            aspect_dir: "/c/aspect".into(),
            log: "/c/sync.log".into(),
            echo: false,
        }
    }

    #[test]
    fn argv_puts_startup_flag_first_and_common_flags_on_every_command() {
        let b = bazel(Some("/c/output_base"));
        for command in ["info", "mod", "build", "query"] {
            let argv = b.argv(command, &["x".into()]);
            assert_eq!(argv[0], "--output_base=/c/output_base");
            assert_eq!(argv[1], command);
            assert!(argv.contains(&"--lockfile_mode=off".to_string()));
            assert!(argv.contains(&"--inject_repository=bazel_kotlin_lsp=/c/aspect".to_string()));
            assert_eq!(argv.last().unwrap(), "x");
        }
    }

    #[test]
    fn argv_without_dedicated_output_base() {
        let argv = bazel(None).argv("info", &[]);
        assert_eq!(argv[0], "info");
        assert!(!argv.iter().any(|a| a.starts_with("--output_base")));
    }

    #[test]
    fn build_args_carry_aspect_and_pattern_file() {
        let args = Bazel::build_args(
            Path::new("/c/targets.txt"),
            "bkl_json",
            Some(Path::new("/c/bep.json")),
        );
        for expected in [
            "--symlink_prefix=/",
            "--keep_going",
            "--aspects=@bazel_kotlin_lsp//:aspect.bzl%bkl_aspect",
            "--output_groups=bkl_json",
            "--remote_download_outputs=toplevel",
            "--target_pattern_file=/c/targets.txt",
            "--build_event_json_file=/c/bep.json",
        ] {
            assert!(
                args.contains(&expected.to_string()),
                "{expected} missing from {args:?}"
            );
        }
        assert!(
            !Bazel::build_args(Path::new("/t"), "bkl_jars", None)
                .iter()
                .any(|a| a.starts_with("--build_event"))
        );
    }

    #[test]
    fn classify_build_table() {
        assert_eq!(classify_build(0, 3), BuildOutcome::Full);
        assert_eq!(classify_build(0, 0), BuildOutcome::Failed);
        assert_eq!(classify_build(1, 3), BuildOutcome::Partial);
        assert_eq!(classify_build(1, 0), BuildOutcome::Failed);
        assert_eq!(classify_build(2, 3), BuildOutcome::Failed);
        assert_eq!(classify_build(37, 3), BuildOutcome::Failed);
    }

    #[test]
    fn resolve_binary_reports_all_candidates() {
        let err = resolve_binary_in(None, Some(std::ffi::OsString::new()))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("bazelisk") && err.contains("bazel") && err.contains("--bazel"),
            "{err}"
        );
    }

    #[test]
    fn resolve_binary_prefers_explicit_then_bazelisk() {
        let t = tempfile::tempdir().unwrap();
        for name in ["bazelisk", "bazel"] {
            std::fs::write(t.path().join(name), "").unwrap();
        }
        let path = Some(t.path().as_os_str().to_owned());
        assert_eq!(
            resolve_binary_in(Some(Path::new("/x/bazel")), path.clone()).unwrap(),
            PathBuf::from("/x/bazel")
        );
        assert_eq!(
            resolve_binary_in(None, path).unwrap(),
            t.path().join("bazelisk")
        );
    }
}
