//! Running Bazel with the flags every sync invocation shares.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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
    /// Runs each command in its own process group, so that cancelling interrupts it.
    pub cancel: Option<Cancel>,
}

/// Stops a sync's Bazel commands: interrupts the running one and refuses to start more.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<CancelState>);

#[derive(Debug, Default)]
struct CancelState {
    cancelled: AtomicBool,
    running: Mutex<Vec<u32>>,
}

impl Cancel {
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::SeqCst);
        for &pid in self.0.running.lock().unwrap().iter() {
            interrupt_group(pid);
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    fn track(&self, pid: u32) -> Tracked<'_> {
        self.0.running.lock().unwrap().push(pid);
        if self.is_cancelled() {
            interrupt_group(pid);
        }
        Tracked(self, pid)
    }
}

struct Tracked<'a>(&'a Cancel, u32);

impl Drop for Tracked<'_> {
    fn drop(&mut self) {
        self.0.0.running.lock().unwrap().retain(|&p| p != self.1);
    }
}

/// SIGINT to the whole group, as a terminal's Ctrl-C would: the Bazel client then cancels its command.
fn interrupt_group(pid: u32) {
    // SAFETY: kill(2) has no memory-safety preconditions.
    unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGINT) };
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
/// The Bazel binary to run in `workspace`, with its version: the explicit one, else the first `bazelisk`, then
/// `bazel`, on `PATH` whose `--version` works there. Version-manager shims (mise, asdf) exist on `PATH` even where
/// they have no version configured, so finding the file is not enough.
pub fn find_bazel(
    explicit: Option<&Path>,
    workspace: &Path,
    path_var: Option<OsString>,
) -> anyhow::Result<(PathBuf, Option<BazelVersion>)> {
    if let Some(explicit) = explicit {
        let version = probe_version(explicit, workspace).map_err(anyhow::Error::msg)?;
        return Ok((explicit.to_path_buf(), version));
    }
    let dirs: Vec<PathBuf> = std::env::split_paths(&path_var.unwrap_or_default()).collect();
    let candidates = ["bazelisk", "bazel"]
        .iter()
        .flat_map(|name| dirs.iter().map(move |d| d.join(name)))
        .filter(|p| p.is_file());
    let mut failures = Vec::new();
    for candidate in candidates {
        match probe_version(&candidate, workspace) {
            Ok(version) => return Ok((candidate, version)),
            Err(why) => failures.push(why),
        }
    }
    if failures.is_empty() {
        bail!("neither bazelisk nor bazel is on PATH; install one or pass --bazel <path>");
    }
    bail!(
        "no working Bazel on PATH in {}; fix one or pass --bazel <path>:\n  {}",
        workspace.display(),
        failures.join("\n  ")
    )
}

fn probe_version(binary: &Path, workspace: &Path) -> Result<Option<BazelVersion>, String> {
    let out = Command::new(binary)
        .arg("--version")
        .current_dir(workspace)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", binary.display()))?;
    if !out.status.success() {
        return Err(format!(
            "{} --version failed: {}",
            binary.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(parse_bazel_version(&String::from_utf8_lossy(&out.stdout)))
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
            "--norun_validations".into(),
            format!("--target_pattern_file={}", patterns_file.display()),
        ];
        if let Some(bep) = bep {
            args.push(format!("--build_event_json_file={}", bep.display()));
        }
        args
    }

    fn check_cancelled(&self) -> anyhow::Result<()> {
        if self.cancel.as_ref().is_some_and(Cancel::is_cancelled) {
            bail!("sync cancelled");
        }
        Ok(())
    }

    /// Runs one Bazel command in the workspace, appending the command line and all of stderr to the log.
    pub fn run(&self, command: &str, args: &[String]) -> anyhow::Result<BazelRun> {
        self.check_cancelled()?;
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

        let mut command = Command::new(&self.binary);
        command
            .args(&argv)
            .current_dir(&self.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if self.cancel.is_some() {
            command.process_group(0);
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("starting {}", self.binary.display()))?;
        let _tracked = self.cancel.as_ref().map(|c| c.track(child.id()));
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
            cancel: None,
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
            "--norun_validations",
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

    /// A folder holding executable scripts, as (name, shell body).
    fn bin_dir(scripts: &[(&str, &str)]) -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in scripts {
            let path = dir.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        dir
    }

    const BROKEN_SHIM: &str = "echo 'mise ERROR No version is set for shim: bazelisk' >&2; exit 1";

    fn path_of(dirs: &[&tempfile::TempDir]) -> Option<OsString> {
        Some(std::env::join_paths(dirs.iter().map(|d| d.path())).unwrap())
    }

    #[test]
    fn broken_bazelisk_shim_falls_back_to_bazel() {
        let bin = bin_dir(&[("bazelisk", BROKEN_SHIM), ("bazel", "echo 'bazel 9.2.0'")]);
        let ws = tempfile::tempdir().unwrap();
        let (found, version) = find_bazel(None, ws.path(), path_of(&[&bin])).unwrap();
        assert_eq!(found, bin.path().join("bazel"));
        assert_eq!(version.unwrap().to_string(), "9.2.0");
    }

    #[test]
    fn a_later_working_bazelisk_on_path_wins_over_bazel() {
        let shims = bin_dir(&[("bazelisk", BROKEN_SHIM)]);
        let real = bin_dir(&[
            ("bazelisk", "echo 'bazel 9.3.2'"),
            ("bazel", "echo 'bazel 8.0.0'"),
        ]);
        let ws = tempfile::tempdir().unwrap();
        let (found, _) = find_bazel(None, ws.path(), path_of(&[&shims, &real])).unwrap();
        assert_eq!(found, real.path().join("bazelisk"));
    }

    #[test]
    fn version_is_probed_in_the_workspace() {
        let bin = bin_dir(&[(
            "bazelisk",
            "[ -f MODULE.bazel ] && echo 'bazel 9.3.2' || { echo 'not here' >&2; exit 1; }",
        )]);
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(ws.path().join("MODULE.bazel"), "").unwrap();
        let (found, _) = find_bazel(None, ws.path(), path_of(&[&bin])).unwrap();
        assert_eq!(found, bin.path().join("bazelisk"));
    }

    #[test]
    fn no_working_bazel_lists_every_candidate() {
        let bin = bin_dir(&[
            ("bazelisk", BROKEN_SHIM),
            ("bazel", "echo 'bazel shim broken too' >&2; exit 2"),
        ]);
        let ws = tempfile::tempdir().unwrap();
        let err = format!(
            "{:#}",
            find_bazel(None, ws.path(), path_of(&[&bin])).unwrap_err()
        );
        for expected in [
            bin.path().join("bazelisk").display().to_string(),
            "No version is set for shim".to_string(),
            bin.path().join("bazel").display().to_string(),
            "bazel shim broken too".to_string(),
            "--bazel".to_string(),
        ] {
            assert!(err.contains(&expected), "{expected} missing from: {err}");
        }
        let none = format!(
            "{:#}",
            find_bazel(None, ws.path(), Some(OsString::new())).unwrap_err()
        );
        assert!(
            none.contains("neither bazelisk nor bazel is on PATH") && none.contains("--bazel"),
            "{none}"
        );
    }

    #[test]
    fn explicit_binary_is_used_and_never_replaced() {
        let bin = bin_dir(&[
            ("mine", "echo 'bazel 9.2.0'"),
            ("broken", BROKEN_SHIM),
            ("bazelisk", "echo 'bazel 9.3.2'"),
        ]);
        let ws = tempfile::tempdir().unwrap();
        let path = path_of(&[&bin]);
        let mine = bin.path().join("mine");
        assert_eq!(
            find_bazel(Some(&mine), ws.path(), path.clone()).unwrap().0,
            mine
        );
        let err = format!(
            "{:#}",
            find_bazel(Some(&bin.path().join("broken")), ws.path(), path).unwrap_err()
        );
        assert!(err.contains("No version is set for shim"), "{err}");
    }
}
