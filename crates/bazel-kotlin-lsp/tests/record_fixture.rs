mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use bkl_core::aspect_template::{ASPECT, ASPECT_REPO, JSON_GROUP, RuleRepos, render_aspect};

fn aspect_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repos = RuleRepos {
        rules_java: "rules_java+".into(),
        rules_kotlin: "rules_kotlin+".into(),
    };
    for (name, content) in render_aspect(&repos) {
        std::fs::write(dir.path().join(name), content).unwrap();
    }
    dir
}

/// Label -> JSON file, from the BEP's named file sets.
fn aspect_outputs(bep: &str) -> BTreeMap<String, PathBuf> {
    let mut out = BTreeMap::new();
    for line in bep.lines() {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        for file in event["namedSetOfFiles"]["files"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let name = file["name"].as_str().unwrap_or_default();
            let uri = file["uri"].as_str().unwrap_or_default();
            if let (true, Some(path)) = (name.ends_with(".bkl.json"), uri.strip_prefix("file://")) {
                let path = PathBuf::from(path.replace("%20", " "));
                let json: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                out.insert(json["label"].as_str().unwrap().to_string(), path);
            }
        }
    }
    out
}

#[test]
#[ignore = "needs Bazel and network"]
fn fixture_aspect_records_expected_targets() {
    let fixture = support::fixture_copy();
    let aspect = aspect_dir();
    let out = tempfile::tempdir().unwrap();
    let bep = out.path().join("bep.json");
    let status = Command::new(support::bazelisk())
        .current_dir(fixture.path())
        .args([
            "build",
            "--symlink_prefix=/",
            "--lockfile_mode=off",
            "--keep_going",
        ])
        .arg(format!(
            "--inject_repository={ASPECT_REPO}={}",
            aspect.path().display()
        ))
        .arg(format!("--aspects={ASPECT}"))
        .arg(format!("--output_groups={JSON_GROUP}"))
        .arg(format!("--build_event_json_file={}", bep.display()))
        .args([
            "--",
            "//...",
            "-//modules/...",
            "@@greeting+//...",
            "@@shapes+//...",
        ])
        .arg("@@rules_kotlin+//kotlin/compiler:kotlin-stdlib")
        .status()
        .unwrap();
    assert!(status.success());

    let outputs = aspect_outputs(&std::fs::read_to_string(&bep).unwrap());
    for label in [
        "@@//app:app",
        "@@//app:app_test",
        "@@//gen:version",
        "@@//deps:serialization",
        "@@greeting+//:greeting",
        "@@shapes+//shapes:shapes",
        "@@shapes+//shapes:shapes_test",
        "@@rules_kotlin+//kotlin/compiler:kotlin-stdlib",
    ] {
        assert!(
            outputs.contains_key(label),
            "no aspect output for {label}: {:?}",
            outputs.keys()
        );
    }
    assert!(
        outputs
            .keys()
            .any(|l| l.contains("kotlinx_serialization_core_jvm")),
        "{:?}",
        outputs.keys()
    );
    assert!(!outputs.keys().any(|l| l.starts_with("@@//odd")));

    if std::env::var_os("BKL_RECORD").is_some() {
        let dest = support::repo_root().join("crates/bkl-core/tests/data/fixture-aspect");
        for entry in std::fs::read_dir(&dest).unwrap() {
            std::fs::remove_file(entry.unwrap().path()).unwrap();
        }
        for (label, path) in &outputs {
            let file: String = label
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            let file = file.trim_matches('_').to_string() + ".bkl.json";
            let json: serde_json::Value =
                serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            std::fs::write(
                dest.join(file),
                serde_json::to_string_pretty(&json).unwrap() + "\n",
            )
            .unwrap();
        }
    }
}
