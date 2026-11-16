use std::process::Command;

use bazel_kotlin_lsp::sync::{SyncOptions, run_sync};

fn options(dir: &std::path::Path) -> SyncOptions {
    SyncOptions {
        start_dir: dir.to_path_buf(),
        cache_root: Some(dir.join("cache")),
        ..Default::default()
    }
}

#[test]
fn not_a_bazel_repo_errors() {
    let t = tempfile::tempdir().unwrap();
    let err = run_sync(&options(t.path())).unwrap_err();
    assert!(
        err.to_string().contains("not inside a Bazel workspace"),
        "{err}"
    );
}

#[test]
fn workspace_only_repo_errors() {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("WORKSPACE"), "").unwrap();
    let err = run_sync(&options(t.path())).unwrap_err();
    assert!(
        err.to_string()
            .contains("WORKSPACE-only repositories are not supported"),
        "{err}"
    );
}

#[test]
fn cli_exit_code_and_stdout_on_error() {
    let t = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_bazel-kotlin-lsp"))
        .args(["sync", "--workspace"])
        .arg(t.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not inside a Bazel workspace"));
}

#[test]
fn dot_dot_in_start_dir_is_normalised() {
    let t = tempfile::tempdir().unwrap();
    std::fs::create_dir(t.path().join("sub")).unwrap();
    let mut options = options(t.path());
    options.start_dir = t.path().join("sub/..");
    let err = run_sync(&options).unwrap_err().to_string();
    assert!(!err.contains(".."), "{err}");
    assert!(err.contains(&t.path().display().to_string()), "{err}");
}

const GOOD_INFO: &str =
    r#"echo "output_base: $ROOT/ob"; echo "execution_root: $ROOT/ob/execroot/_main""#;
const GOOD_GRAPH: &str = r#"echo '{"key":"<root>","name":"demo","dependencies":[{"key":"rules_kotlin@2.4.21","name":"rules_kotlin","dependencies":[]}]}'"#;
const GOOD_MAPPING: &str = r#"echo '{"":"","rules_kotlin":"rules_kotlin+"}'"#;

/// A Bazel stand-in: each command runs the given shell snippet; `--build_event_json_file` is in `$BEP`.
struct FakeBazel {
    info: &'static str,
    graph: &'static str,
    build: &'static str,
}

impl Default for FakeBazel {
    fn default() -> Self {
        FakeBazel {
            info: GOOD_INFO,
            graph: GOOD_GRAPH,
            build: "exit 0",
        }
    }
}

impl FakeBazel {
    fn install(&self, dir: &std::path::Path) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script = format!(
            r#"#!/bin/sh
ROOT='{root}'
[ "$1" = --version ] && {{ echo "bazel 9.3.2"; exit 0; }}
cmd=; BEP=
for a in "$@"; do
  case "$a" in
    --build_event_json_file=*) BEP="${{a#--build_event_json_file=}}" ;;
    --*) ;;
    *) [ -z "$cmd" ] && cmd="$a" ;;
  esac
done
case "$cmd" in
  info) {info} ;;
  mod) case " $* " in *" graph "*) {graph} ;; *) {mapping} ;; esac ;;
  build) {build} ;;
esac
"#,
            root = dir.display(),
            info = self.info,
            graph = self.graph,
            mapping = GOOD_MAPPING,
            build = self.build,
        );
        let path = dir.join("fake-bazel");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
}

struct Failure {
    err: String,
    log: std::path::PathBuf,
    bazel: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

/// Runs a sync of an empty Bzlmod repo with the fake Bazel, which must fail.
fn failing_sync(fake: FakeBazel) -> Failure {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().to_path_buf();
    std::fs::write(dir.join("MODULE.bazel"), "").unwrap();
    let bazel = fake.install(&dir);
    let out = dir.join("out");
    let mut opts = options(&dir);
    opts.bazel = Some(bazel.clone());
    opts.out_dir = Some(out.clone());
    let err = run_sync(&opts).unwrap_err().to_string();
    Failure {
        err,
        log: out.join("sync.log"),
        bazel,
        _dir: t,
    }
}

/// The error names the command to reproduce and the log, and the log holds that command line.
fn assert_reports(f: &Failure, command: &str) {
    let reproduce = f
        .err
        .lines()
        .find_map(|l| l.strip_prefix("Reproduce: "))
        .unwrap_or_else(|| panic!("no Reproduce line: {}", f.err));
    assert!(
        reproduce.starts_with(&f.bazel.display().to_string()),
        "{reproduce}"
    );
    assert!(reproduce.contains("--output_base="), "{reproduce}");
    assert!(reproduce.contains(&format!(" {command} ")), "{reproduce}");
    assert!(
        f.err.contains(&format!("Log: {}", f.log.display())),
        "{}",
        f.err
    );
    let log_text = std::fs::read_to_string(&f.log).unwrap();
    assert!(log_text.contains(&format!("$ {reproduce}")), "{log_text}");
}

#[test]
fn failed_bazel_command_shows_stderr_command_and_log() {
    let f = failing_sync(FakeBazel {
        info: "echo 'ERROR: fake info failure' >&2; exit 2",
        ..Default::default()
    });
    assert!(f.err.contains("`bazel info` failed (exit 2)"), "{}", f.err);
    assert!(f.err.contains("ERROR: fake info failure"), "{}", f.err);
    assert_reports(&f, "info");
    assert!(
        std::fs::read_to_string(&f.log)
            .unwrap()
            .contains("ERROR: fake info failure")
    );
}

#[test]
fn failed_build_shows_stderr_command_and_log() {
    let f = failing_sync(FakeBazel {
        build: "echo 'ERROR: fake build failure' >&2; exit 2",
        ..Default::default()
    });
    assert!(f.err.contains("Bazel build failed (exit 2)"), "{}", f.err);
    assert!(f.err.contains("ERROR: fake build failure"), "{}", f.err);
    assert_reports(&f, "build");
}

#[test]
fn truncated_build_event_file_still_shows_bazel_error() {
    let f = failing_sync(FakeBazel {
        build: r#"printf '{"id":{"progress":{}},"progre' > "$BEP"; echo 'FATAL: fake crash' >&2; exit 37"#,
        ..Default::default()
    });
    assert!(f.err.contains("Bazel build failed (exit 37)"), "{}", f.err);
    assert!(f.err.contains("FATAL: fake crash"), "{}", f.err);
    assert_reports(&f, "build");
}

#[test]
fn unreadable_build_event_file_after_a_build_names_the_build() {
    let f = failing_sync(FakeBazel {
        build: r#"printf 'not json\n' > "$BEP"; exit 0"#,
        ..Default::default()
    });
    assert!(f.err.contains("build event file"), "{}", f.err);
    assert_reports(&f, "build");
}

#[test]
fn unexpected_info_output_names_the_command() {
    let f = failing_sync(FakeBazel {
        info: "echo nothing useful",
        ..Default::default()
    });
    assert!(f.err.contains("output_base"), "{}", f.err);
    assert_reports(&f, "info");
}

#[test]
fn unexpected_mod_graph_output_names_the_command() {
    let f = failing_sync(FakeBazel {
        graph: "echo 'not json'",
        ..Default::default()
    });
    assert!(f.err.contains("bazel mod graph"), "{}", f.err);
    assert_reports(&f, "mod");
}
