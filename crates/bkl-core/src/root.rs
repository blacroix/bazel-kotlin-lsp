//! Finding the Bazel workspace root that contains a folder.

use std::path::{Path, PathBuf};

use crate::bazel_mod::{normalize, string_assignment};

const MARKERS: [&str; 4] = ["MODULE.bazel", "REPO.bazel", "WORKSPACE", "WORKSPACE.bazel"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BazelRoot {
    Bzlmod(PathBuf),
    WorkspaceOnly(PathBuf),
}

/// The nearest enclosing workspace root. A module folder used through `local_path_override` by an enclosing
/// workspace resolves to that enclosing workspace.
pub fn find_bazel_root(start: &Path) -> Option<BazelRoot> {
    let found = start
        .ancestors()
        .find(|dir| MARKERS.iter().any(|m| dir.join(m).is_file()))?;
    if !found.join("MODULE.bazel").is_file() {
        return Some(BazelRoot::WorkspaceOnly(found.to_path_buf()));
    }
    let overriding = found
        .ancestors()
        .skip(1)
        .find(|ancestor| overrides(ancestor, found));
    Some(BazelRoot::Bzlmod(overriding.unwrap_or(found).to_path_buf()))
}

fn overrides(ancestor: &Path, module_dir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(ancestor.join("MODULE.bazel")) else {
        return false;
    };
    let Ok(rel) = module_dir.strip_prefix(ancestor) else {
        return false;
    };
    text.lines()
        .filter_map(|line| string_assignment(line.trim()))
        .any(|(key, value)| key == "path" && normalize(Path::new(&value)) == normalize(rel))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn module_dir_resolves_to_overriding_root() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        touch(
            &root.join("MODULE.bazel"),
            "bazel_dep(name = \"m\")\nlocal_path_override(\n    module_name = \"m\",\n    path = \"mods/m\",\n)\n",
        );
        touch(&root.join("mods/m/MODULE.bazel"), "module(name = \"m\")\n");
        fs::create_dir_all(root.join("mods/m/src")).unwrap();
        assert_eq!(
            find_bazel_root(&root.join("mods/m/src")),
            Some(BazelRoot::Bzlmod(root.to_path_buf()))
        );
    }

    #[test]
    fn unrelated_nested_module_stays_its_own_root() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        touch(&root.join("MODULE.bazel"), "module(name = \"outer\")\n");
        touch(&root.join("mods/m/MODULE.bazel"), "module(name = \"m\")\n");
        fs::create_dir_all(root.join("mods/m/src")).unwrap();
        assert_eq!(
            find_bazel_root(&root.join("mods/m/src")),
            Some(BazelRoot::Bzlmod(root.join("mods/m")))
        );
    }

    #[test]
    fn workspace_only_detected() {
        let t = tempfile::tempdir().unwrap();
        touch(&t.path().join("WORKSPACE"), "");
        fs::create_dir_all(t.path().join("a/b")).unwrap();
        assert_eq!(
            find_bazel_root(&t.path().join("a/b")),
            Some(BazelRoot::WorkspaceOnly(t.path().to_path_buf()))
        );
    }

    #[test]
    fn no_marker_returns_none() {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir_all(t.path().join("a")).unwrap();
        assert_eq!(find_bazel_root(&t.path().join("a")), None);
    }
}
