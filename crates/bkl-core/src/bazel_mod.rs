//! Parsing of `bazel mod` output: the module graph, repository definitions and repository mappings.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, bail};

pub use crate::aspect_template::RuleRepos;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModGraph {
    pub root_name: String,
    /// Modules with a non-registry override (`<name>@_`), such as `local_path_override`.
    pub overridden: Vec<String>,
    pub all: Vec<String>,
}

/// Parses `bazel mod graph --output=json`.
pub fn parse_mod_graph(json: &str) -> anyhow::Result<ModGraph> {
    let root: serde_json::Value =
        serde_json::from_str(json).context("parsing `bazel mod graph` output")?;
    let root_name = root
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let mut overridden = BTreeSet::new();
    let mut all = BTreeSet::from([root_name.clone()]);
    let mut stack: Vec<&serde_json::Value> = vec![&root];
    while let Some(node) = stack.pop() {
        for key in ["dependencies", "indirectDependencies"] {
            for dep in node
                .get(key)
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                let name = dep.get("name").and_then(|v| v.as_str()).unwrap_or_default();
                let module_key = dep.get("key").and_then(|v| v.as_str()).unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                all.insert(name.to_string());
                if module_key.ends_with("@_") && name != root_name {
                    overridden.insert(name.to_string());
                }
                stack.push(dep);
            }
        }
    }
    Ok(ModGraph {
        root_name,
        overridden: overridden.into_iter().collect(),
        all: all.into_iter().collect(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoDef {
    pub canonical: String,
    pub rule: String,
    pub path: Option<String>,
}

/// Parses `bazel mod show_repo` text output (Bazel 8 and 9 formats).
pub fn parse_show_repo(text: &str) -> Vec<RepoDef> {
    let mut defs = Vec::new();
    let mut current: Option<RepoDef> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("load(") {
            continue;
        }
        if let Some(rule) = line
            .strip_suffix('(')
            .filter(|r| !r.contains(' ') && !r.is_empty())
        {
            current = Some(RepoDef {
                canonical: String::new(),
                rule: rule.to_string(),
                path: None,
            });
        } else if line == ")" {
            defs.extend(current.take().filter(|d| !d.canonical.is_empty()));
        } else if let (Some(def), Some((key, value))) = (current.as_mut(), string_assignment(line))
        {
            match key {
                "name" => def.canonical = value,
                "path" => def.path = Some(value),
                _ => {}
            }
        }
    }
    defs
}

/// `key = "value",` -> (key, value).
pub(crate) fn string_assignment(line: &str) -> Option<(&str, String)> {
    let (key, rest) = line.split_once('=')?;
    let value = rest.trim().trim_end_matches(',').trim();
    let value = value.strip_prefix('"')?.strip_suffix('"')?;
    Some((key.trim(), value.to_string()))
}

/// Parses one line of `bazel mod dump_repo_mapping` output (apparent name -> canonical name).
pub fn parse_repo_mapping(line: &str) -> anyhow::Result<BTreeMap<String, String>> {
    serde_json::from_str(line).context("parsing `bazel mod dump_repo_mapping` output")
}

/// Canonical names of rules_java and rules_kotlin, as the aspect must load them.
pub fn rule_repos(
    root_mapping: &BTreeMap<String, String>,
    graph: &ModGraph,
) -> anyhow::Result<RuleRepos> {
    let find = |name: &str| -> Option<String> {
        root_mapping.get(name).cloned().or_else(|| {
            graph
                .all
                .iter()
                .any(|m| m == name)
                .then(|| format!("{name}+"))
        })
    };
    let Some(rules_kotlin) = find("rules_kotlin") else {
        bail!("rules_kotlin is not in the module graph; is this a Kotlin project?");
    };
    let rules_java = find("rules_java").unwrap_or_else(|| "rules_java+".to_string());
    Ok(RuleRepos {
        rules_java,
        rules_kotlin,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalModule {
    pub name: String,
    pub canonical: String,
    /// Relative to the workspace root; may start with `..`.
    pub rel_path: PathBuf,
    pub abs_path: PathBuf,
}

/// The `local_repository` definitions with a path, sorted by name.
pub fn local_modules(defs: &[RepoDef], root: &Path) -> Vec<LocalModule> {
    let mut modules: Vec<LocalModule> = defs
        .iter()
        .filter(|d| d.rule == "local_repository")
        .filter_map(|d| {
            let rel = PathBuf::from(d.path.as_ref()?);
            Some(LocalModule {
                name: d.canonical.trim_end_matches('+').to_string(),
                canonical: d.canonical.clone(),
                abs_path: normalize(&root.join(&rel)),
                rel_path: rel,
            })
        })
        .collect();
    modules.sort_by(|a, b| a.name.cmp(&b.name));
    modules
}

/// Lexical normalization: removes `.` and resolves `..` without touching the filesystem.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRAPH: &str = r#"{
  "key": "<root>",
  "name": "demo",
  "version": "",
  "apparentName": "demo",
  "dependencies": [
    {
      "key": "util@_",
      "name": "util",
      "version": "",
      "apparentName": "util",
      "dependencies": [
        {"key": "rules_kotlin@2.4.21", "name": "rules_kotlin", "version": "2.4.21", "dependencies": [], "indirectDependencies": [], "cycles": []}
      ],
      "indirectDependencies": [],
      "cycles": []
    },
    {"key": "rules_java@9.9.0", "name": "rules_java", "version": "9.9.0", "dependencies": [], "indirectDependencies": [], "cycles": []}
  ],
  "indirectDependencies": [],
  "cycles": [],
  "root": true
}"#;

    const SHOW_REPO_8: &str =
        "## util@_:\nlocal_repository(\n  name = \"util+\",\n  path = \"libs/util\",\n)\n\n";
    const SHOW_REPO_9: &str = "## util@_:\nload(\"@@bazel_tools//tools/build_defs/repo:local.bzl\", \"local_repository\")\nlocal_repository(\n  name = \"util+\",\n  path = \"libs/util\",\n)\n\n";

    #[test]
    fn mod_graph_marks_overrides() {
        let g = parse_mod_graph(GRAPH).unwrap();
        assert_eq!(g.root_name, "demo");
        assert_eq!(g.overridden, ["util"]);
        assert_eq!(g.all, ["demo", "rules_java", "rules_kotlin", "util"]);
    }

    #[test]
    fn show_repo_bazel8_and_9() {
        let expected = RepoDef {
            canonical: "util+".into(),
            rule: "local_repository".into(),
            path: Some("libs/util".into()),
        };
        assert_eq!(
            parse_show_repo(SHOW_REPO_8),
            std::slice::from_ref(&expected)
        );
        assert_eq!(parse_show_repo(SHOW_REPO_9), [expected]);
    }

    #[test]
    fn repo_mapping_root() {
        let m =
            parse_repo_mapping(r#"{"":"","demo":"","util":"util+","bazel_tools":"bazel_tools"}"#)
                .unwrap();
        assert_eq!(m["util"], "util+");
        assert_eq!(m[""], "");
    }

    #[test]
    fn rule_repos_prefers_mapping() {
        let g = parse_mod_graph(GRAPH).unwrap();
        let mapping = BTreeMap::from([
            ("rules_kotlin".to_string(), "rules_kotlin+".to_string()),
            ("rules_java".to_string(), "rj~".to_string()),
        ]);
        let r = rule_repos(&mapping, &g).unwrap();
        assert_eq!(
            r,
            RuleRepos {
                rules_java: "rj~".into(),
                rules_kotlin: "rules_kotlin+".into()
            }
        );
    }

    #[test]
    fn rule_repos_falls_back_to_plus_suffix() {
        let g = parse_mod_graph(GRAPH).unwrap();
        let r = rule_repos(&BTreeMap::new(), &g).unwrap();
        assert_eq!(
            r,
            RuleRepos {
                rules_java: "rules_java+".into(),
                rules_kotlin: "rules_kotlin+".into()
            }
        );
    }

    #[test]
    fn rule_repos_requires_rules_kotlin() {
        let g = ModGraph {
            root_name: "demo".into(),
            overridden: vec![],
            all: vec!["demo".into(), "rules_java".into()],
        };
        let err = rule_repos(&BTreeMap::new(), &g).unwrap_err();
        assert!(
            err.to_string()
                .contains("rules_kotlin is not in the module graph")
        );
    }

    #[test]
    fn local_modules_from_defs() {
        let defs = vec![
            RepoDef {
                canonical: "util+".into(),
                rule: "local_repository".into(),
                path: Some("libs/util".into()),
            },
            RepoDef {
                canonical: "far+".into(),
                rule: "local_repository".into(),
                path: Some("../far".into()),
            },
            RepoDef {
                canonical: "git+".into(),
                rule: "git_repository".into(),
                path: None,
            },
        ];
        let mods = local_modules(&defs, Path::new("/ws"));
        assert_eq!(mods.len(), 2);
        assert_eq!(mods[0].name, "far");
        assert_eq!(mods[0].abs_path, PathBuf::from("/far"));
        assert_eq!(
            mods[1],
            LocalModule {
                name: "util".into(),
                canonical: "util+".into(),
                rel_path: "libs/util".into(),
                abs_path: "/ws/libs/util".into()
            }
        );
    }
}
