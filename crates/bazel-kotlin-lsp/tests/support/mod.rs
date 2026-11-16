#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use bazel_kotlin_lsp::sync::SyncOptions;

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// A git-initialised copy of `fixtures/basic`, with rule versions overridden by `BKL_FIXTURE_VERSIONS`
/// (e.g. `rules_kotlin=2.1.0,rules_java=8.6.0,rules_jvm_external=6.6`).
pub fn fixture_copy() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    copy_tree(&repo_root().join("fixtures/basic"), dir.path());
    if let Ok(spec) = std::env::var("BKL_FIXTURE_VERSIONS") {
        for module_file in module_files(dir.path()) {
            let mut text = std::fs::read_to_string(&module_file).unwrap();
            for pair in spec.split(',').filter(|p| !p.is_empty()) {
                let (name, version) = pair.split_once('=').expect("name=version");
                text = override_version(&text, name, version);
            }
            std::fs::write(&module_file, text).unwrap();
        }
    }
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["add", "-A"],
        &["commit", "-q", "-m", "fixture"],
    ] {
        let status = Command::new("git")
            .args([
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir.path())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }
    dir
}

/// Bazelisk from `BAZELISK`, else the first `bazelisk` on PATH.
pub fn bazelisk() -> PathBuf {
    if let Some(p) = std::env::var_os("BAZELISK") {
        return PathBuf::from(p);
    }
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|d| d.join("bazelisk"))
        .find(|p| p.is_file())
        .expect("bazelisk not found: set BAZELISK or run through `mise run integration`")
}

fn module_files(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.join("MODULE.bazel")];
    for entry in std::fs::read_dir(root.join("modules")).unwrap() {
        out.push(entry.unwrap().path().join("MODULE.bazel"));
    }
    out
}

fn override_version(text: &str, name: &str, version: &str) -> String {
    let needle = format!("bazel_dep(name = \"{name}\", version = \"");
    let Some(start) = text.find(&needle) else {
        return text.to_string();
    };
    let from = start + needle.len();
    let end = from + text[from..].find('"').unwrap();
    format!("{}{}{}", &text[..from], version, &text[end..])
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// A fixture copy plus its own cache root. Dropping it stops the Bazel server of the dedicated output base and
/// makes Bazel's read-only output tree removable.
pub struct Synced {
    pub fixture: tempfile::TempDir,
    pub cache: tempfile::TempDir,
}

impl Synced {
    pub fn new() -> Self {
        Synced {
            fixture: fixture_copy(),
            cache: tempfile::tempdir().unwrap(),
        }
    }

    pub fn options(&self) -> SyncOptions {
        SyncOptions {
            start_dir: self.fixture.path().to_path_buf(),
            bazel: Some(bazelisk()),
            cache_root: Some(self.cache.path().to_path_buf()),
            ..Default::default()
        }
    }
}

impl Drop for Synced {
    fn drop(&mut self) {
        let bases = glob_output_bases(self.cache.path());
        for base in &bases {
            let _ = Command::new(bazelisk())
                .arg(format!("--output_base={}", base.display()))
                .arg("shutdown")
                .current_dir(self.fixture.path())
                .status();
        }
        make_writable(self.cache.path());
    }
}

fn glob_output_bases(cache: &Path) -> Vec<PathBuf> {
    let root = cache.join("bazel-kotlin-lsp");
    std::fs::read_dir(&root)
        .into_iter()
        .flatten()
        .map(|e| e.unwrap().path().join("output_base"))
        .filter(|p| p.is_dir())
        .collect()
}

fn make_writable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
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

pub fn workspace_json(model_dir: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(model_dir.join("workspace.json")).unwrap()).unwrap()
}

pub fn module<'a>(ws: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    ws["modules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == name)
        .unwrap_or_else(|| panic!("no module {name}"))
}

pub fn dep_names(module: &serde_json::Value) -> Vec<String> {
    module["dependencies"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["name"].as_str().map(str::to_string))
        .collect()
}
