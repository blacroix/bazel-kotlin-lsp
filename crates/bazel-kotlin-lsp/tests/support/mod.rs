#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

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
