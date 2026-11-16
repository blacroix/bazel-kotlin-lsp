use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use bkl_core::cache::write_owner;

fn gc(cache: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bazel-kotlin-lsp"))
        .arg("gc")
        .arg("--cache-root")
        .arg(cache)
        .args(args)
        .output()
        .unwrap()
}

fn folder(cache: &Path, key: &str, workspace: &Path) -> PathBuf {
    let dir = cache.join("bazel-kotlin-lsp").join(key);
    std::fs::create_dir_all(dir.join("model")).unwrap();
    std::fs::write(dir.join("model/workspace.json"), "{}").unwrap();
    write_owner(&dir, workspace, 0).unwrap();
    dir
}

#[test]
fn gc_lists_then_removes_the_caches_of_deleted_workspaces() {
    let cache = tempfile::tempdir().unwrap();
    let live = tempfile::tempdir().unwrap();
    let stale = folder(cache.path(), "stale", Path::new("/nonexistent/worktree"));
    let kept = folder(cache.path(), "kept", live.path());

    let dry = gc(cache.path(), &["--dry-run"]);
    assert_eq!(dry.status.code(), Some(0));
    let text = String::from_utf8_lossy(&dry.stdout);
    assert!(
        text.contains("would remove") && text.contains("/nonexistent/worktree"),
        "{text}"
    );
    assert!(stale.exists());

    let run = gc(cache.path(), &[]);
    assert_eq!(
        run.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let text = String::from_utf8_lossy(&run.stdout);
    assert!(
        text.contains("removed") && text.contains(&stale.display().to_string()),
        "{text}"
    );
    assert!(!stale.exists() && kept.exists());

    let again = gc(cache.path(), &[]);
    assert!(String::from_utf8_lossy(&again.stdout).contains("nothing to remove"));
}

#[test]
fn gc_workspace_removes_that_workspace_cache_now() {
    let cache = tempfile::tempdir().unwrap();
    let live = tempfile::tempdir().unwrap();
    let mine = folder(cache.path(), "mine", live.path());
    let run = gc(
        cache.path(),
        &["--workspace", live.path().to_str().unwrap()],
    );
    assert_eq!(
        run.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(!mine.exists());
}
