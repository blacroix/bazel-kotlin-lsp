//! Where sync keeps its outputs, and how it writes them.

use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The folder under the cache root that holds one folder per workspace.
const CACHE_DIR: &str = "bazel-kotlin-lsp";
const OWNER_FILE: &str = "owner.json";
/// Prefix of a workspace folder renamed for deletion; an interrupted deletion leaves it behind.
pub const TRASH_PREFIX: &str = ".trash-";
const DAY_SECS: u64 = 24 * 3600;
/// A cache whose workspace and its parent folder are both missing is kept this long after its last use, in case
/// they come back (a drive that is not mounted).
const GONE_GRACE_SECS: u64 = 3 * DAY_SECS;
/// A cache from before owner records is removed once it has not been touched this long.
const UNOWNED_GRACE_SECS: u64 = 14 * DAY_SECS;

/// `$XDG_CACHE_HOME` or `~/.cache` on Linux, `~/Library/Caches` on macOS. `None` without an absolute base, so a
/// missing `HOME` never puts the cache under the current directory.
pub fn cache_root(env: &dyn Fn(&str) -> Option<String>, macos: bool) -> Option<PathBuf> {
    let absolute = |k: &str| env(k).map(PathBuf::from).filter(|p| p.is_absolute());
    if macos {
        return absolute("HOME").map(|h| h.join("Library/Caches"));
    }
    absolute("XDG_CACHE_HOME").or_else(|| absolute("HOME").map(|h| h.join(".cache")))
}

/// First 16 hex characters of the SHA-256 of the workspace root's path.
pub fn workspace_key(root_realpath: &Path) -> String {
    digest_hex(root_realpath.as_os_str().as_bytes())
}

/// First 16 hex characters of the SHA-256 of `bytes`.
pub fn digest_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheLayout {
    pub dir: PathBuf,
}

impl CacheLayout {
    pub fn for_workspace(cache_root: &Path, root_realpath: &Path) -> Self {
        CacheLayout {
            dir: cache_root
                .join(CACHE_DIR)
                .join(workspace_key(root_realpath)),
        }
    }

    pub fn output_base(&self) -> PathBuf {
        self.dir.join("output_base")
    }

    pub fn aspect_dir(&self) -> PathBuf {
        self.dir.join("aspect")
    }

    pub fn patterns_file(&self) -> PathBuf {
        self.dir.join("targets.txt")
    }

    pub fn jar_patterns_file(&self) -> PathBuf {
        self.dir.join("jar-targets.txt")
    }

    /// kotlin-lsp's index for this workspace, kept here so removing the folder removes it too.
    pub fn kotlin_lsp_index_dir(&self) -> PathBuf {
        self.dir.join("kotlin-lsp-index")
    }

    pub fn scope_decision_file(&self) -> PathBuf {
        self.dir.join("scope-decision.json")
    }
}

/// Which workspace a cache folder belongs to; the folder name is only a hash of its path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    pub workspace: PathBuf,
    /// Seconds since the Unix epoch.
    pub last_used: u64,
}

pub fn write_owner(dir: &Path, workspace: &Path, now_secs: u64) -> std::io::Result<()> {
    let owner = Owner {
        workspace: workspace.to_path_buf(),
        last_used: now_secs,
    };
    let bytes = serde_json::to_vec_pretty(&owner).map_err(std::io::Error::other)?;
    write_if_changed(&dir.join(OWNER_FILE), &bytes).map(|_| ())
}

pub fn read_owner(dir: &Path) -> Option<Owner> {
    serde_json::from_slice(&std::fs::read(dir.join(OWNER_FILE)).ok()?).ok()
}

/// Why a cache folder can be removed, or `None` to keep it. `modified_secs` is the folder's own modification time,
/// used when it has no owner record.
pub fn staleness(
    owner: Option<&Owner>,
    modified_secs: u64,
    now_secs: u64,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<String> {
    let days = |since: u64| now_secs.saturating_sub(since) / DAY_SECS;
    match owner {
        Some(owner) if exists(&owner.workspace) => None,
        // Its parent folder is there, so the workspace was deleted, not merely unreachable.
        Some(owner) if owner.workspace.parent().is_some_and(exists) => Some(format!(
            "workspace {} was deleted",
            owner.workspace.display()
        )),
        Some(owner) => (now_secs.saturating_sub(owner.last_used) >= GONE_GRACE_SECS).then(|| {
            format!(
                "workspace {} is unreachable, unused for {} days",
                owner.workspace.display(),
                days(owner.last_used)
            )
        }),
        None => (now_secs.saturating_sub(modified_secs) >= UNOWNED_GRACE_SECS)
            .then(|| format!("no owner record, unused for {} days", days(modified_secs))),
    }
}

/// The workspace folders and the leftover trash folders under the cache root, each sorted.
pub fn cache_dirs(cache_root: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut dirs = Vec::new();
    let mut trash = Vec::new();
    for entry in std::fs::read_dir(cache_root.join(CACHE_DIR))
        .into_iter()
        .flatten()
        .flatten()
    {
        let path = entry.path();
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(TRASH_PREFIX)
        {
            trash.push(path);
        } else {
            dirs.push(path);
        }
    }
    dirs.sort();
    trash.sort();
    (dirs, trash)
}

/// Holds `<dir>/sync.lock` exclusively until the returned file is dropped, waiting for any other holder.
pub fn lock_exclusive(dir: &Path) -> std::io::Result<std::fs::File> {
    std::fs::create_dir_all(dir)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("sync.lock"))?;
    file.lock()?;
    Ok(file)
}

/// Writes `bytes` unless the file already holds them; returns whether it wrote. The write is a rename of a
/// temporary file in the same folder, so readers never see a partial file.
pub fn write_if_changed(path: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    if std::fs::read(path).is_ok_and(|current| current == bytes) {
        return Ok(false);
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!("{name}.tmp-{}", std::process::id()));
    let result = std::fs::File::create(&tmp)
        .and_then(|mut f| f.write_all(bytes))
        .and_then(|_| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map(|_| true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn cache_root_linux_xdg_then_home() {
        assert_eq!(
            cache_root(&env(&[("XDG_CACHE_HOME", "/x"), ("HOME", "/h")]), false),
            Some(PathBuf::from("/x"))
        );
        assert_eq!(
            cache_root(&env(&[("XDG_CACHE_HOME", ""), ("HOME", "/h")]), false),
            Some(PathBuf::from("/h/.cache"))
        );
        assert_eq!(
            cache_root(
                &env(&[("XDG_CACHE_HOME", "relative"), ("HOME", "/h")]),
                false
            ),
            Some(PathBuf::from("/h/.cache"))
        );
    }

    #[test]
    fn cache_root_macos() {
        assert_eq!(
            cache_root(&env(&[("XDG_CACHE_HOME", "/x"), ("HOME", "/h")]), true),
            Some(PathBuf::from("/h/Library/Caches"))
        );
    }

    #[test]
    fn cache_root_is_never_relative() {
        for macos in [true, false] {
            assert_eq!(cache_root(&env(&[]), macos), None);
            assert_eq!(cache_root(&env(&[("HOME", "")]), macos), None);
            assert_eq!(cache_root(&env(&[("HOME", "relative")]), macos), None);
        }
        assert_eq!(
            cache_root(&env(&[("XDG_CACHE_HOME", "/x")]), false),
            Some(PathBuf::from("/x"))
        );
        assert_eq!(cache_root(&env(&[("XDG_CACHE_HOME", "/x")]), true), None);
    }

    #[test]
    fn workspace_key_is_16_hex_and_stable() {
        let k = workspace_key(Path::new("/ws/repo"));
        assert_eq!(k.len(), 16);
        assert!(k.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(k, workspace_key(Path::new("/ws/repo")));
        assert_ne!(k, workspace_key(Path::new("/ws/other")));
    }

    #[test]
    fn layout_paths() {
        let l = CacheLayout::for_workspace(Path::new("/c"), Path::new("/ws/repo"));
        let key = workspace_key(Path::new("/ws/repo"));
        assert_eq!(l.dir, PathBuf::from(format!("/c/bazel-kotlin-lsp/{key}")));
        assert_eq!(l.output_base(), l.dir.join("output_base"));
        assert_eq!(l.aspect_dir(), l.dir.join("aspect"));
        assert_eq!(l.patterns_file(), l.dir.join("targets.txt"));
        assert_eq!(l.jar_patterns_file(), l.dir.join("jar-targets.txt"));
    }

    #[test]
    fn write_if_changed_skips_identical_bytes() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("sub/workspace.json");
        assert!(write_if_changed(&p, b"one").unwrap());
        let before = std::fs::metadata(&p).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(!write_if_changed(&p, b"one").unwrap());
        assert_eq!(std::fs::metadata(&p).unwrap().modified().unwrap(), before);
        assert!(write_if_changed(&p, b"two").unwrap());
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
    }

    #[test]
    fn sync_lock_is_exclusive() {
        let t = tempfile::tempdir().unwrap();
        let held = lock_exclusive(&t.path().join("sub")).unwrap();
        let other = std::fs::File::open(t.path().join("sub/sync.lock")).unwrap();
        assert!(matches!(
            other.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(held);
        assert!(other.try_lock().is_ok());
    }

    #[test]
    fn write_if_changed_replaces_atomically() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("workspace.json");
        write_if_changed(&p, b"a").unwrap();
        write_if_changed(&p, b"b").unwrap();
        let names: Vec<String> = std::fs::read_dir(t.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["workspace.json"]);
    }

    #[test]
    fn owner_round_trips_and_names_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_owner(dir.path()).is_none());
        write_owner(dir.path(), Path::new("/ws/repo"), 1_000).unwrap();
        let owner = read_owner(dir.path()).unwrap();
        assert_eq!(owner.workspace, PathBuf::from("/ws/repo"));
        assert_eq!(owner.last_used, 1_000);
    }

    const DAY: u64 = 24 * 3600;

    #[test]
    fn stale_when_the_workspace_is_gone_and_unused_for_days() {
        let owner = Owner {
            workspace: "/ws/gone".into(),
            last_used: 0,
        };
        let gone = |_: &Path| false;
        let here = |_: &Path| true;
        let reason = staleness(Some(&owner), 0, 3 * DAY, &gone).unwrap();
        assert!(reason.contains("/ws/gone"), "{reason}");
        assert_eq!(
            staleness(Some(&owner), 0, 2 * DAY, &gone),
            None,
            "used recently"
        );
        assert_eq!(
            staleness(Some(&owner), 0, 100 * DAY, &here),
            None,
            "workspace exists"
        );
    }

    #[test]
    fn a_deleted_workspace_whose_parent_exists_is_stale_at_once() {
        let owner = Owner {
            workspace: "/worktrees/repo/gone".into(),
            last_used: 10 * DAY,
        };
        let parent_only = |p: &Path| p == Path::new("/worktrees/repo");
        let reason = staleness(Some(&owner), 0, 10 * DAY, &parent_only).unwrap();
        assert!(
            reason.contains("deleted") && reason.contains("/worktrees/repo/gone"),
            "{reason}"
        );
    }

    #[test]
    fn a_cache_without_owner_is_stale_only_after_two_weeks() {
        let any = |_: &Path| true;
        assert_eq!(staleness(None, 0, 13 * DAY, &any), None);
        assert!(staleness(None, 0, 14 * DAY, &any).is_some());
    }

    #[test]
    fn cache_dirs_lists_workspaces_and_trash_separately() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("bazel-kotlin-lsp");
        for d in ["aaaa", "bbbb", ".trash-cccc-1"] {
            std::fs::create_dir_all(base.join(d)).unwrap();
        }
        std::fs::write(base.join("stray-file"), "").unwrap();
        let (dirs, trash) = cache_dirs(root.path());
        assert_eq!(dirs, [base.join("aaaa"), base.join("bbbb")]);
        assert_eq!(trash, [base.join(".trash-cccc-1")]);
        assert_eq!(cache_dirs(&root.path().join("missing")), (vec![], vec![]));
    }

    #[test]
    fn layout_has_a_kotlin_lsp_index_folder() {
        let layout = CacheLayout::for_workspace(Path::new("/c"), Path::new("/ws"));
        assert_eq!(
            layout.kotlin_lsp_index_dir(),
            layout.dir.join("kotlin-lsp-index")
        );
    }
}
