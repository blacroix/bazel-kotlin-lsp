//! Times sync on a generated repository: cold, no-op, incremental, and time to the first definition.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use serde_json::{Value, json};

pub struct BenchArgs {
    pub repo: PathBuf,
    pub sync_bin: PathBuf,
    pub kotlin_lsp: Option<PathBuf>,
    pub out: PathBuf,
}

struct Phase {
    wall_ms: u128,
    summary: Value,
    out_dir: PathBuf,
}

fn sync(args: &BenchArgs, cache: &Path) -> anyhow::Result<Phase> {
    let start = Instant::now();
    let out = Command::new(&args.sync_bin)
        .args(["sync", "--workspace"])
        .arg(&args.repo)
        .arg("--cache-root")
        .arg(cache)
        .output()
        .with_context(|| format!("running {}", args.sync_bin.display()))?;
    let wall_ms = start.elapsed().as_millis();
    if !out.status.success() {
        bail!(
            "sync failed ({}):\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let out_dir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let summary = serde_json::from_slice(&std::fs::read(out_dir.join("sync-summary.json"))?)?;
    Ok(Phase {
        wall_ms,
        summary,
        out_dir,
    })
}

/// Total size of the regular files under `path`, without following symlinks.
pub fn tree_bytes(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.is_dir() {
        std::fs::read_dir(path)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| tree_bytes(&e.path()))
            .sum()
    } else if meta.is_file() {
        meta.len()
    } else {
        0
    }
}

fn add_extra_package(repo: &Path) -> anyhow::Result<()> {
    let dir = repo.join("mods/m00/extra");
    std::fs::create_dir_all(dir.join("src/main/kotlin/synth/m00/extra"))?;
    std::fs::write(
        dir.join("BUILD.bazel"),
        "load(\"@rules_kotlin//kotlin:jvm.bzl\", \"kt_jvm_library\")\n\nkt_jvm_library(\n    name = \"extra\",\n    srcs = [\"src/main/kotlin/synth/m00/extra/Extra.kt\"],\n    deps = [\"//t000\"],\n)\n",
    )?;
    std::fs::write(
        dir.join("src/main/kotlin/synth/m00/extra/Extra.kt"),
        "package synth.m00.extra\n\nclass Extra\n",
    )?;
    Ok(())
}

fn first_definition(args: &BenchArgs, kotlin_lsp: &Path, model_dir: &Path) -> anyhow::Result<u128> {
    let scratch = tempfile::tempdir()?;
    let app = std::fs::read_to_string(args.repo.join("app/src/main/kotlin/synth/app/App.kt"))?;
    let import = app
        .lines()
        .find(|l| l.starts_with("import synth.m00."))
        .context("App.kt has no m00 import")?;
    let class = import.rsplit('.').next().unwrap_or_default();
    let cases = scratch.path().join("cases.json");
    std::fs::write(
        &cases,
        json!([{"name": "app -> m00", "file": "app/src/main/kotlin/synth/app/App.kt", "line_contains": import, "symbol": class, "expect": "mods/m00/"}]).to_string(),
    )?;
    let start = Instant::now();
    let results = bkl_probe::run(&bkl_probe::ProbeArgs {
        root: args.repo.clone(),
        cases,
        model_dir: model_dir.to_path_buf(),
        index_dir: Some(scratch.path().join("index")),
        timeout: Duration::from_secs(900),
        log: Some(scratch.path().join("server.log")),
        server: vec![
            kotlin_lsp.to_string_lossy().into_owned(),
            "--stdio".into(),
            format!("--system-path={}", scratch.path().join("system").display()),
        ],
    })?;
    if !results.iter().all(|r| r.pass) {
        bail!("first definition did not resolve: {results:?}");
    }
    Ok(start.elapsed().as_millis())
}

fn shutdown_bazel(output_base: &Path, repo: &Path) {
    let bazelisk = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|d| d.join("bazelisk"))
        .find(|p| p.is_file());
    if let Some(bazelisk) = bazelisk {
        let _ = Command::new(bazelisk)
            .arg(format!("--output_base={}", output_base.display()))
            .arg("shutdown")
            .current_dir(repo)
            .status();
    }
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

pub fn bench(args: &BenchArgs) -> anyhow::Result<Value> {
    let cache = tempfile::tempdir()?;
    let cold = sync(args, cache.path())?;
    let output_base = cold.out_dir.join("output_base");
    let output_base_bytes = tree_bytes(&output_base);
    let noop = sync(args, cache.path())?;
    add_extra_package(&args.repo)?;
    let incremental = sync(args, cache.path())?;
    let first_definition_ms = match &args.kotlin_lsp {
        Some(k) => Some(first_definition(args, k, &incremental.out_dir)?),
        None => None,
    };
    shutdown_bazel(&output_base, &args.repo);
    make_writable(cache.path());

    let mut phases = BTreeMap::new();
    for (name, phase) in [
        ("cold", &cold),
        ("noop", &noop),
        ("incremental", &incremental),
    ] {
        phases.insert(name, json!({"wall_ms": phase.wall_ms, "timings_ms": phase.summary["timings_ms"], "changed": phase.summary["changed"], "modules": phase.summary["modules"], "libraries": phase.summary["libraries"]}));
    }
    let report = json!({"phases": phases, "output_base_bytes": output_base_bytes, "first_definition_ms": first_definition_ms});
    std::fs::write(&args.out, serde_json::to_vec_pretty(&report)?)?;
    Ok(report)
}

/// The report as a Markdown table.
pub fn markdown(report: &Value) -> String {
    let mut out = String::from(
        "| phase | wall (s) | bazel setup | aspect build | jar build | model (ms) | write (ms) | changed |\n|---|---|---|---|---|---|---|---|\n",
    );
    for name in ["cold", "noop", "incremental"] {
        let p = &report["phases"][name];
        let t = &p["timings_ms"];
        let secs = |v: &Value| format!("{:.1}", v.as_f64().unwrap_or(0.0) / 1000.0);
        out += &format!(
            "| {name} | {} | {} | {} | {} | {} | {} | {} |\n",
            secs(&p["wall_ms"]),
            secs(&t["bazel_setup"]),
            secs(&t["aspect_build"]),
            secs(&t["jar_build"]),
            t["model"],
            t["write"],
            p["changed"]
        );
    }
    out += &format!(
        "\nModules: {}, libraries: {}. Output base: {:.0} MB.",
        report["phases"]["cold"]["modules"],
        report["phases"]["cold"]["libraries"],
        report["output_base_bytes"].as_f64().unwrap_or(0.0) / 1e6
    );
    if let Some(ms) = report["first_definition_ms"].as_u64() {
        out += &format!(
            " First definition (cold kotlin-lsp): {:.1} s.",
            ms as f64 / 1000.0
        );
    }
    out + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_lists_every_phase() {
        let phase = json!({"wall_ms": 1500, "timings_ms": {"bazel_setup": 100, "aspect_build": 1000, "jar_build": 200, "model": 12, "write": 1}, "changed": true, "modules": 2001, "libraries": 3});
        let report = json!({"phases": {"cold": phase, "noop": phase, "incremental": phase}, "output_base_bytes": 2_000_000_000u64, "first_definition_ms": 30000});
        let md = markdown(&report);
        assert_eq!(md.matches("| 1.5 |").count(), 3);
        assert!(md.contains("Output base: 2000 MB"));
        assert!(md.contains("First definition (cold kotlin-lsp): 30.0 s"));
    }

    #[test]
    fn tree_bytes_sums_files_without_following_symlinks() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("d")).unwrap();
        std::fs::write(t.path().join("d/a"), [0u8; 10]).unwrap();
        std::fs::write(t.path().join("b"), [0u8; 5]).unwrap();
        std::os::unix::fs::symlink(t.path().join("d"), t.path().join("link")).unwrap();
        assert_eq!(tree_bytes(t.path()), 15);
    }
}
