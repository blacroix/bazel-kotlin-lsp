//! Which file changes call for a new sync.

use bkl_core::nested::read_bazelignore;
use std::path::{Path, PathBuf};

const NAMES: [&str; 10] = [
    "BUILD",
    "BUILD.bazel",
    "MODULE.bazel",
    "REPO.bazel",
    "WORKSPACE",
    "WORKSPACE.bazel",
    ".bazelrc",
    ".bazelversion",
    ".bazelignore",
    ".bazel-kotlin.toml",
];

pub struct WatchFilter {
    roots: Vec<Root>,
}

struct Root {
    /// The folder as given and its real path: file events may report either.
    paths: Vec<PathBuf>,
    /// Folders relative to the root that Bazel ignores.
    ignored: Vec<PathBuf>,
}

impl Root {
    fn new(dir: &Path, ignored: Vec<PathBuf>) -> Self {
        let mut paths = vec![dir.to_path_buf()];
        if let Ok(real) = dir.canonicalize()
            && real != dir
        {
            paths.push(real);
        }
        Root { paths, ignored }
    }

    fn relative<'a>(&self, path: &'a Path) -> Option<&'a Path> {
        self.paths.iter().find_map(|p| path.strip_prefix(p).ok())
    }
}

impl WatchFilter {
    /// Watches `root`, minus the folders its `.bazelignore` lists.
    pub fn new(root: &Path) -> Self {
        WatchFilter {
            roots: vec![Root::new(root, read_bazelignore(root))],
        }
    }

    /// Also watches a module folder outside the workspace root.
    pub fn add_root(&mut self, dir: &Path) {
        self.roots.push(Root::new(dir, Vec::new()));
    }

    pub fn reload_ignores(&mut self) {
        let main = &mut self.roots[0];
        main.ignored = read_bazelignore(&main.paths[0]);
    }

    pub fn is_relevant(&self, path: &Path) -> bool {
        if !path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(relevant_name)
        {
            return false;
        }
        self.roots.iter().any(|root| {
            root.relative(path).is_some_and(|rel| {
                let mut components = rel.components().map(|c| c.as_os_str());
                let top_level_output = components
                    .next()
                    .and_then(|c| c.to_str())
                    .is_some_and(|c| c.starts_with("bazel-"));
                !top_level_output
                    && !rel.components().any(|c| c.as_os_str() == ".git")
                    && !root.ignored.iter().any(|i| rel.starts_with(i))
            })
        })
    }
}

fn relevant_name(name: &str) -> bool {
    NAMES.contains(&name) || name.ends_with(".bzl") || name.ends_with("_install.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("MODULE.bazel"), "").unwrap();
        t
    }

    #[test]
    fn kt_edit_does_not_sync() {
        let t = repo();
        let f = WatchFilter::new(t.path());
        for name in ["app/App.kt", "app/Main.java", "README.md", "app/BUILD.bak"] {
            assert!(!f.is_relevant(&t.path().join(name)), "{name}");
        }
    }

    #[test]
    fn build_files_and_dependency_locks_sync() {
        let t = repo();
        let f = WatchFilter::new(t.path());
        for name in [
            "app/BUILD",
            "app/BUILD.bazel",
            "MODULE.bazel",
            "REPO.bazel",
            "WORKSPACE",
            "WORKSPACE.bazel",
            "tools/defs.bzl",
            ".bazelrc",
            ".bazelversion",
            ".bazelignore",
            ".bazel-kotlin.toml",
            "maven_install.json",
            "third_party/kotlin_install.json",
        ] {
            assert!(f.is_relevant(&t.path().join(name)), "{name}");
        }
    }

    #[test]
    fn bazelignored_dir_ignored() {
        let t = repo();
        std::fs::write(
            t.path().join(".bazelignore"),
            "# generated\nnode_modules\n\nignored/deep/\n",
        )
        .unwrap();
        let f = WatchFilter::new(t.path());
        for name in [
            "node_modules/x/BUILD",
            "ignored/deep/BUILD.bazel",
            "bazel-out/k8-fastbuild/bin/BUILD",
            "bazel-repo/BUILD",
            ".git/BUILD",
        ] {
            assert!(!f.is_relevant(&t.path().join(name)), "{name}");
        }
        for name in [
            "ignored/BUILD",
            "node_modules_x/BUILD",
            "app/bazel-out/BUILD",
        ] {
            assert!(f.is_relevant(&t.path().join(name)), "{name}");
        }
    }

    #[test]
    fn paths_are_matched_through_the_real_root() {
        let t = repo();
        let link = tempfile::tempdir().unwrap();
        let linked = link.path().join("repo");
        std::os::unix::fs::symlink(t.path(), &linked).unwrap();
        std::fs::write(t.path().join(".bazelignore"), "skip\n").unwrap();
        let f = WatchFilter::new(&linked);
        let real = t.path().canonicalize().unwrap();
        assert!(!f.is_relevant(&real.join("skip/BUILD")));
        assert!(f.is_relevant(&real.join("app/BUILD")));
        assert!(!f.is_relevant(Path::new("/elsewhere/BUILD")));
    }

    #[test]
    fn added_roots_are_watched_without_the_main_ignores() {
        let t = repo();
        std::fs::write(t.path().join(".bazelignore"), "lib\n").unwrap();
        let module = tempfile::tempdir().unwrap();
        let mut f = WatchFilter::new(t.path());
        assert!(!f.is_relevant(&module.path().join("lib/BUILD")));
        f.add_root(module.path());
        assert!(f.is_relevant(&module.path().join("lib/BUILD")));
        assert!(!f.is_relevant(&module.path().join("lib/A.kt")));
    }
}
