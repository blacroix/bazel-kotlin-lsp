//! Bazel workspaces that sit inside another one, such as worktrees some tools keep inside the checkout.
//!
//! `//...` loads their packages as packages of the outer workspace, against an outer MODULE.bazel that may lack
//! their repositories.

use std::path::{Path, PathBuf};

const WORKSPACE_FILES: [&str; 4] = ["MODULE.bazel", "REPO.bazel", "WORKSPACE", "WORKSPACE.bazel"];
const BUILD_FILES: [&str; 2] = ["BUILD", "BUILD.bazel"];

/// The folders `.bazelignore` lists, relative to `root`.
pub fn read_bazelignore(root: &Path) -> Vec<PathBuf> {
    std::fs::read_to_string(root.join(".bazelignore"))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| PathBuf::from(l.trim_end_matches('/')))
        .collect()
}

/// Folders under `root`, relative to it, holding a workspace of their own with at least one package: excluding a
/// folder without packages is an error to Bazel. Symlinks, `.git` and `.bazelignore`d folders are not entered.
pub fn nested_workspaces(root: &Path) -> Vec<PathBuf> {
    let ignored = read_bazelignore(root);
    let mut found = Vec::new();
    let mut pending = subdirs(root);
    while let Some(dir) = pending.pop() {
        let rel = dir
            .strip_prefix(root)
            .expect("under the root")
            .to_path_buf();
        if ignored.iter().any(|i| rel.starts_with(i)) {
            continue;
        }
        if has_any(&dir, &WORKSPACE_FILES) {
            if has_package(&dir) {
                found.push(rel);
            }
            continue;
        }
        pending.extend(subdirs(&dir));
    }
    found.sort();
    found
}

fn has_any(dir: &Path, names: &[&str]) -> bool {
    names.iter().any(|n| dir.join(n).is_file())
}

fn has_package(dir: &Path) -> bool {
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        if has_any(&dir, &BUILD_FILES) {
            return true;
        }
        pending.extend(subdirs(&dir));
    }
    false
}

/// Real subfolders, not symlinks, except `.git`.
fn subdirs(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()) && e.file_name() != ".git")
        .map(|e| e.path())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(root: &Path, rel: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    #[test]
    fn workspaces_with_packages_are_found_without_entering_them() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        for file in [
            "MODULE.bazel",
            "app/BUILD.bazel",
            ".delta/worktrees/a/repo/MODULE.bazel",
            ".delta/worktrees/a/repo/testing/BUILD.bazel",
            ".delta/worktrees/a/repo/inner/MODULE.bazel",
            ".delta/worktrees/a/repo/inner/BUILD",
            "legacy/WORKSPACE",
            "legacy/BUILD",
            "standalone/MODULE.bazel",
            "standalone/docs/README.md",
            "ignored/MODULE.bazel",
            "ignored/BUILD.bazel",
            ".git/modules/x/MODULE.bazel",
            ".git/modules/x/BUILD.bazel",
        ] {
            touch(root, file);
        }
        std::fs::write(root.join(".bazelignore"), "# generated\nignored/\n").unwrap();
        assert_eq!(
            nested_workspaces(root),
            [
                PathBuf::from(".delta/worktrees/a/repo"),
                PathBuf::from("legacy")
            ]
        );
    }

    #[test]
    fn symlinked_workspaces_are_not_followed() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("repo");
        touch(&root, "MODULE.bazel");
        touch(t.path(), "elsewhere/MODULE.bazel");
        touch(t.path(), "elsewhere/BUILD");
        std::os::unix::fs::symlink(t.path().join("elsewhere"), root.join("bazel-out")).unwrap();
        assert!(nested_workspaces(&root).is_empty());
    }
}
