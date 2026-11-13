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
