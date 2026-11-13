//! The aspect's per-target JSON: the contract between `aspect/aspect.bzl.tpl` and the model builder.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Context;
use serde::Deserialize;

/// One JVM target as seen by the aspect. Paths are execroot-relative, labels are `str(Label)`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TargetInfo {
    pub label: String,
    pub repo: String,
    pub package: String,
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub srcs: Vec<String>,
    #[serde(default)]
    pub generated_srcs: Vec<String>,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub exports: Vec<String>,
    #[serde(default)]
    pub associates: Vec<String>,
    #[serde(default)]
    pub class_jars: Vec<String>,
    #[serde(default)]
    pub source_jars: Vec<String>,
    #[serde(default)]
    pub import_jars: Vec<String>,
    #[serde(default)]
    pub import_srcjar: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub kotlinc: Option<KotlincInfo>,
    #[serde(default)]
    pub java_home: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct KotlincInfo {
    #[serde(default)]
    pub language_version: Option<String>,
    #[serde(default)]
    pub api_version: Option<String>,
    #[serde(default)]
    pub jvm_target: Option<String>,
    #[serde(default)]
    pub opt_in: Vec<String>,
    #[serde(default)]
    pub plugin_classpath: Vec<String>,
    #[serde(default)]
    pub plugin_options: Vec<String>,
}

/// Reads aspect JSON files, sorted by label. A label seen twice (another configuration) keeps the file that
/// sorts first by path.
pub fn read_targets(files: &[PathBuf]) -> anyhow::Result<Vec<TargetInfo>> {
    let mut files = files.to_vec();
    files.sort();
    let mut by_label = BTreeMap::new();
    for file in files {
        let bytes = std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
        let target: TargetInfo = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing {}", file.display()))?;
        by_label.entry(target.label.clone()).or_insert(target);
    }
    Ok(by_label.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn recorded() -> Vec<PathBuf> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/fixture-aspect");
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect()
    }

    #[test]
    fn reads_recorded_fixture() {
        let targets = read_targets(&recorded()).unwrap();
        let labels: Vec<&str> = targets.iter().map(|t| t.label.as_str()).collect();
        let mut sorted = labels.clone();
        sorted.sort();
        assert_eq!(labels, sorted);
        let app = targets.iter().find(|t| t.label == "@@//app:app").unwrap();
        assert_eq!(
            app.kotlinc.as_ref().unwrap().opt_in,
            ["kotlinx.serialization.ExperimentalSerializationApi"]
        );
        assert_eq!(app.repo, "");
        assert!(app.srcs.iter().any(|s| s.ends_with("App.kt")));
    }

    #[test]
    fn duplicate_label_keeps_first() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bkl.json");
        let b = dir.path().join("b.bkl.json");
        std::fs::write(
            &a,
            r#"{"label":"@@//x:x","repo":"","package":"x","name":"x","kind":"first"}"#,
        )
        .unwrap();
        std::fs::write(
            &b,
            r#"{"label":"@@//x:x","repo":"","package":"x","name":"x","kind":"second"}"#,
        )
        .unwrap();
        let targets = read_targets(&[b, a]).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].kind, "first");
    }
}
