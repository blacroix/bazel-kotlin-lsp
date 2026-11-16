//! `gc`: removing the cache folders of workspaces that no longer exist.

use std::fs::{File, TryLockError};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, UNIX_EPOCH};

use bkl_core::bazel_mod::normalize;
use bkl_core::cache::{TRASH_PREFIX, cache_dirs, read_owner, staleness, workspace_key};

/// How long a Bazel server may take to exit after SIGTERM, before SIGKILL.
const SERVER_STOP_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct GcOptions {
    pub cache_root: PathBuf,
    pub dry_run: bool,
    /// Remove this workspace's cache now, whatever its state; nothing else.
    pub workspace: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Removed,
    WouldRemove,
    Skipped(String),
}

#[derive(Debug, Clone)]
pub struct GcEntry {
    pub dir: PathBuf,
    pub reason: String,
    pub bytes: u64,
    pub action: Action,
}

/// Removes (or lists, with `dry_run`) the cache folders that are stale, or the one of `workspace`.
pub fn gc(opts: &GcOptions, now_secs: u64) -> anyhow::Result<Vec<GcEntry>> {
    let (dirs, trash) = cache_dirs(&opts.cache_root);
    let mut targets: Vec<(PathBuf, String)> = Vec::new();
    match &opts.workspace {
        Some(workspace) => {
            let wanted = std::path::absolute(workspace).map(|p| normalize(&p))?;
            let key = workspace.canonicalize().ok().map(|p| workspace_key(&p));
            for dir in dirs {
                let owned = read_owner(&dir).is_some_and(|o| normalize(&o.workspace) == wanted);
                let named = key
                    .as_deref()
                    .is_some_and(|k| dir.file_name() == Some(k.as_ref()));
                if owned || named {
                    targets.push((dir, format!("requested for {}", wanted.display())));
                }
            }
        }
        None => {
            for dir in dirs {
                let modified = modified_secs(&dir);
                if let Some(reason) =
                    staleness(read_owner(&dir).as_ref(), modified, now_secs, &|p| {
                        p.exists()
                    })
                {
                    targets.push((dir, reason));
                }
            }
            targets.extend(
                trash
                    .into_iter()
                    .map(|t| (t, "an interrupted removal".into())),
            );
        }
    }
    Ok(targets
        .into_iter()
        .map(|(dir, reason)| {
            let bytes = tree_bytes(&dir);
            let action = if opts.dry_run {
                Action::WouldRemove
            } else {
                match remove_cache_dir(&dir) {
                    Ok(()) => Action::Removed,
                    Err(why) => Action::Skipped(why),
                }
            };
            GcEntry {
                dir,
                reason,
                bytes,
                action,
            }
        })
        .collect())
}

/// One line per entry, then the total, for the `gc` command.
pub fn report(entries: &[GcEntry]) -> String {
    if entries.is_empty() {
        return "nothing to remove\n".into();
    }
    let mut out = String::new();
    let mut freed = 0;
    for e in entries {
        let size = megabytes(e.bytes);
        let line = match &e.action {
            Action::Removed => {
                freed += e.bytes;
                format!("removed {} ({size}): {}", e.dir.display(), e.reason)
            }
            Action::WouldRemove => {
                freed += e.bytes;
                format!("would remove {} ({size}): {}", e.dir.display(), e.reason)
            }
            Action::Skipped(why) => format!("skipped {}: {why}", e.dir.display()),
        };
        out.push_str(&line);
        out.push('\n');
    }
    out.push_str(&format!("total {}\n", megabytes(freed)));
    out
}

fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1e6)
}

/// Stops the folder's Bazel server, then deletes the folder; never while a sync holds its lock.
fn remove_cache_dir(dir: &Path) -> Result<(), String> {
    let lock = File::open(dir.join("sync.lock")).ok();
    if let Some(lock) = &lock {
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err("in use by a running sync".into()),
            Err(TryLockError::Error(e)) => return Err(format!("locking: {e}")),
        }
    }
    stop_server(&dir.join("output_base"));
    let name = dir.file_name().unwrap_or_default().to_string_lossy();
    let trash = if name.starts_with(TRASH_PREFIX) {
        dir.to_path_buf()
    } else {
        let nanos = UNIX_EPOCH.elapsed().map_or(0, |d| d.as_nanos());
        let trash = dir.with_file_name(format!(
            "{TRASH_PREFIX}{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::rename(dir, &trash).map_err(|e| format!("renaming: {e}"))?;
        trash
    };
    drop(lock);
    make_writable(&trash);
    std::fs::remove_dir_all(&trash).map_err(|e| format!("deleting {}: {e}", trash.display()))
}

/// Stops the Bazel server of `output_base`, if one runs: SIGTERM, then SIGKILL.
fn stop_server(output_base: &Path) {
    let Some(pid) = std::fs::read_to_string(output_base.join("server/server.pid.txt"))
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
    else {
        return;
    };
    // The pid file outlives the server, and the pid may now belong to another process.
    let is_server =
        || command_line(pid).is_some_and(|c| c.contains(&*output_base.to_string_lossy()));
    if !is_server() {
        return;
    }
    // SAFETY: kill(2) has no memory-safety preconditions.
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let deadline = Instant::now() + SERVER_STOP_WAIT;
    while is_server() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    if is_server() {
        // SAFETY: as above.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

fn command_line(pid: i32) -> Option<String> {
    let out = Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn modified_secs(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

/// Total size of the regular files under `path`, without following symlinks.
fn tree_bytes(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return if meta.is_file() { meta.len() } else { 0 };
    }
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| tree_bytes(&e.path()))
        .sum()
}

/// Bazel leaves read-only folders in its output tree, which `remove_dir_all` cannot empty.
fn make_writable(path: &Path) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if meta.is_dir() {
        let _ = std::fs::set_permissions(
            path,
            std::fs::Permissions::from_mode(meta.permissions().mode() | 0o700),
        );
        for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
            make_writable(&entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Child;

    use bkl_core::cache::{lock_exclusive, write_owner};

    const DAY: u64 = 24 * 3600;
    const NOW: u64 = 100 * DAY;

    struct Cache {
        root: tempfile::TempDir,
    }

    impl Cache {
        fn new() -> Self {
            Cache {
                root: tempfile::tempdir().unwrap(),
            }
        }

        /// A workspace folder owned by `workspace`, last used `last_used`, with a read-only output tree.
        fn folder(&self, key: &str, workspace: &Path, last_used: u64) -> PathBuf {
            let dir = self.root.path().join("bazel-kotlin-lsp").join(key);
            let external = dir.join("output_base/external/repo");
            std::fs::create_dir_all(&external).unwrap();
            std::fs::write(external.join("lib.jar"), "jar bytes").unwrap();
            std::fs::set_permissions(&external, std::fs::Permissions::from_mode(0o555)).unwrap();
            std::fs::create_dir_all(dir.join("model")).unwrap();
            write_owner(&dir, workspace, last_used).unwrap();
            dir
        }

        fn options(&self) -> GcOptions {
            GcOptions {
                cache_root: self.root.path().to_path_buf(),
                dry_run: false,
                workspace: None,
            }
        }
    }

    fn removed(entries: &[GcEntry]) -> Vec<&Path> {
        entries
            .iter()
            .filter(|e| e.action == Action::Removed)
            .map(|e| e.dir.as_path())
            .collect()
    }

    #[test]
    fn removes_a_stale_cache_and_keeps_the_others() {
        let c = Cache::new();
        let live = tempfile::tempdir().unwrap();
        let stale = c.folder("stale", Path::new("/nonexistent/worktree"), 0);
        let recent = c.folder("recent", Path::new("/nonexistent/other"), NOW - DAY);
        let kept = c.folder("live", live.path(), 0);

        let entries = gc(&c.options(), NOW).unwrap();
        assert_eq!(removed(&entries), [stale.as_path()]);
        let entry = &entries[0];
        assert!(entry.reason.contains("/nonexistent/worktree"), "{entry:?}");
        assert!(entry.bytes >= 9, "{entry:?}");
        assert!(!stale.exists());
        assert!(recent.exists() && kept.exists());
    }

    #[test]
    fn a_just_deleted_worktree_is_removed_at_once() {
        let c = Cache::new();
        let worktrees = tempfile::tempdir().unwrap();
        let deleted = c.folder("deleted", &worktrees.path().join("old-worktree"), NOW);
        let entries = gc(&c.options(), NOW).unwrap();
        assert_eq!(removed(&entries), [deleted.as_path()]);
    }

    #[test]
    fn dry_run_removes_nothing() {
        let c = Cache::new();
        let stale = c.folder("stale", Path::new("/nonexistent/worktree"), 0);
        let mut opts = c.options();
        opts.dry_run = true;
        let entries = gc(&opts, NOW).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, Action::WouldRemove);
        assert!(stale.exists());
    }

    #[test]
    fn a_cache_whose_sync_is_running_is_skipped() {
        let c = Cache::new();
        let stale = c.folder("stale", Path::new("/nonexistent/worktree"), 0);
        let _lock = lock_exclusive(&stale).unwrap();
        let entries = gc(&c.options(), NOW).unwrap();
        assert!(
            matches!(&entries[0].action, Action::Skipped(why) if why.contains("in use")),
            "{entries:?}"
        );
        assert!(stale.exists());
    }

    #[test]
    fn workspace_option_removes_that_cache_now() {
        let c = Cache::new();
        let live = tempfile::tempdir().unwrap();
        let mine = c.folder("mine", live.path(), NOW);
        let other = c.folder("other", Path::new("/nonexistent/other"), 0);
        let mut opts = c.options();
        opts.workspace = Some(live.path().to_path_buf());
        let entries = gc(&opts, NOW).unwrap();
        assert_eq!(removed(&entries), [mine.as_path()]);
        assert!(other.exists(), "only the named workspace is removed");
    }

    #[test]
    fn leftovers_of_an_interrupted_removal_are_removed() {
        let c = Cache::new();
        let trash = c.root.path().join("bazel-kotlin-lsp/.trash-abc-1");
        std::fs::create_dir_all(trash.join("x")).unwrap();
        let entries = gc(&c.options(), NOW).unwrap();
        assert_eq!(removed(&entries), [trash.as_path()]);
        assert!(!trash.exists());
    }

    fn wait_exit(child: &mut Child) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    fn write_pid(output_base: &Path, pid: u32) {
        let server = output_base.join("server");
        std::fs::create_dir_all(&server).unwrap();
        std::fs::write(server.join("server.pid.txt"), pid.to_string()).unwrap();
    }

    #[test]
    fn the_cache_bazel_server_is_stopped() {
        let c = Cache::new();
        let stale = c.folder("stale", Path::new("/nonexistent/worktree"), 0);
        let output_base = stale.join("output_base");
        let mut server = Command::new("sh")
            .args(["-c", "while :; do sleep 1; done", "fake-bazel-server"])
            .arg(format!("--output_base={}", output_base.display()))
            .spawn()
            .unwrap();
        write_pid(&output_base, server.id());
        gc(&c.options(), NOW).unwrap();
        assert!(
            wait_exit(&mut server),
            "the cache's Bazel server is still running"
        );
        assert!(!stale.exists());
    }

    #[test]
    fn a_reused_pid_of_another_process_is_left_alone() {
        let c = Cache::new();
        let stale = c.folder("stale", Path::new("/nonexistent/worktree"), 0);
        let mut other = Command::new("sleep").arg("30").spawn().unwrap();
        write_pid(&stale.join("output_base"), other.id());
        gc(&c.options(), NOW).unwrap();
        assert!(
            other.try_wait().unwrap().is_none(),
            "an unrelated process was killed"
        );
        other.kill().unwrap();
        other.wait().unwrap();
        assert!(!stale.exists());
    }
}
