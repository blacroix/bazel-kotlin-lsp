//! Finding the Bazel workspace root that contains a folder.

use std::path::{Path, PathBuf};

use crate::bazel_mod::normalize;

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
    let Ok(rel) = module_dir.strip_prefix(ancestor) else {
        return false;
    };
    let mut pending = vec![ancestor.join("MODULE.bazel")];
    let mut seen = Vec::new();
    while let Some(file) = pending.pop() {
        if seen.contains(&file) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        if string_arguments(&text, "path")
            .any(|value| normalize(Path::new(&value)) == normalize(rel))
        {
            return true;
        }
        let package = file.parent().unwrap_or(ancestor).to_path_buf();
        pending.extend(
            string_arguments(&text, "include")
                .filter_map(|label| include_path(ancestor, &package, &label)),
        );
        seen.push(file);
    }
    false
}

/// Values of `key = "..."` keyword arguments and of `key("...")` calls, anywhere in Starlark text.
fn string_arguments<'a>(text: &'a str, key: &'a str) -> impl Iterator<Item = String> + 'a {
    text.match_indices(key).filter_map(move |(at, _)| {
        let before = text[..at].chars().next_back();
        if before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.') {
            return None;
        }
        let rest = text[at + key.len()..].trim_start();
        let rest = rest
            .strip_prefix('=')
            .or_else(|| rest.strip_prefix('('))?
            .trim_start();
        let value = rest.strip_prefix('"')?;
        Some(value[..value.find('"')?].to_string())
    })
}

/// The file an `include()` label points to: `//pkg:file` from the workspace root, `:file` from the including
/// file's folder. Labels into other repositories are not followed.
fn include_path(root: &Path, package: &Path, label: &str) -> Option<PathBuf> {
    if let Some(rest) = label.strip_prefix("//") {
        let (pkg, name) = rest.split_once(':')?;
        return Some(root.join(pkg).join(name));
    }
    label.strip_prefix(':').map(|name| package.join(name))
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
    fn single_line_override_is_recognised() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        touch(
            &root.join("MODULE.bazel"),
            "local_path_override(module_name = \"m\", path = \"mods/m\")\n",
        );
        touch(&root.join("mods/m/MODULE.bazel"), "module(name = \"m\")\n");
        assert_eq!(
            find_bazel_root(&root.join("mods/m")),
            Some(BazelRoot::Bzlmod(root.to_path_buf()))
        );
    }

    #[test]
    fn override_in_included_file_is_recognised() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        touch(
            &root.join("MODULE.bazel"),
            "include(\"//bazel:deps.MODULE.bazel\")\n",
        );
        touch(
            &root.join("bazel/deps.MODULE.bazel"),
            "include(\":more.MODULE.bazel\")\n",
        );
        touch(
            &root.join("bazel/more.MODULE.bazel"),
            "local_path_override(\n    module_name = \"m\",\n    path = \"mods/m\")\n",
        );
        touch(&root.join("mods/m/MODULE.bazel"), "module(name = \"m\")\n");
        assert_eq!(
            find_bazel_root(&root.join("mods/m")),
            Some(BazelRoot::Bzlmod(root.to_path_buf()))
        );
    }

    #[test]
    fn other_path_like_keys_do_not_count() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        touch(
            &root.join("MODULE.bazel"),
            "archive_override(module_name = \"x\", strip_path = \"mods/m\")\n",
        );
        touch(&root.join("mods/m/MODULE.bazel"), "module(name = \"m\")\n");
        assert_eq!(
            find_bazel_root(&root.join("mods/m")),
            Some(BazelRoot::Bzlmod(root.join("mods/m")))
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
