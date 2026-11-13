//! Where sync keeps its outputs, and how it writes them.

use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// `$XDG_CACHE_HOME` or `~/.cache` on Linux, `~/Library/Caches` on macOS.
pub fn cache_root(env: &dyn Fn(&str) -> Option<String>, macos: bool) -> PathBuf {
    let home = PathBuf::from(env("HOME").unwrap_or_default());
    if macos {
        return home.join("Library/Caches");
    }
    match env("XDG_CACHE_HOME").map(PathBuf::from) {
        Some(xdg) if xdg.is_absolute() => xdg,
        _ => home.join(".cache"),
    }
}

/// First 16 hex characters of the SHA-256 of the workspace root's path.
pub fn workspace_key(root_realpath: &Path) -> String {
    let digest = Sha256::digest(root_realpath.as_os_str().as_bytes());
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
                .join("bazel-kotlin-lsp")
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
            PathBuf::from("/x")
        );
        assert_eq!(
            cache_root(&env(&[("XDG_CACHE_HOME", ""), ("HOME", "/h")]), false),
            PathBuf::from("/h/.cache")
        );
        assert_eq!(
            cache_root(
                &env(&[("XDG_CACHE_HOME", "relative"), ("HOME", "/h")]),
                false
            ),
            PathBuf::from("/h/.cache")
        );
    }

    #[test]
    fn cache_root_macos() {
        assert_eq!(
            cache_root(&env(&[("XDG_CACHE_HOME", "/x"), ("HOME", "/h")]), true),
            PathBuf::from("/h/Library/Caches")
        );
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
}
