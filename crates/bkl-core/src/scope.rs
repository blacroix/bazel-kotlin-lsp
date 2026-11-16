//! Which directories become source modules, and the Bazel target patterns that cover them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::Deserialize;

use crate::bazel_mod::{LocalModule, normalize};

/// Above this many JVM targets, a folder opened below the workspace root narrows the scope to that folder.
pub const NARROW_THRESHOLD: usize = 500;

/// `.bazel-kotlin.toml`. Directories are relative to the workspace root; a leading `-` excludes one. Without any
/// included directory, the whole repository is included. `targets` are added to the directories.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeConfig {
    #[serde(default)]
    pub directories: Vec<String>,
    #[serde(default)]
    pub targets: Vec<String>,
}

pub fn load_scope_config(path: &Path) -> anyhow::Result<Option<ScopeConfig>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let config = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(config))
}

/// Rewrites `cfg.directories` relative to the workspace root (absolute paths inside it are accepted) and checks
/// that every included directory exists. Excluded directories may be missing.
pub fn resolve_directories(cfg: &ScopeConfig, root: &Path) -> anyhow::Result<ScopeConfig> {
    let mut directories = Vec::new();
    for entry in &cfg.directories {
        let (sign, dir) = match entry.strip_prefix('-') {
            Some(dir) => ("-", dir),
            None => ("", entry.as_str()),
        };
        let path = Path::new(dir.trim_end_matches('/'));
        let rel = if path.is_absolute() {
            match normalize(path).strip_prefix(root) {
                Ok(rel) => rel.to_path_buf(),
                Err(_) => bail!(
                    "directory {dir:?} is outside the workspace root {}",
                    root.display()
                ),
            }
        } else {
            normalize(path)
        };
        if sign.is_empty() && !root.join(&rel).is_dir() {
            bail!(
                "directory {dir:?}: no such folder under the workspace root {} (directories are relative to it)",
                root.display()
            );
        }
        directories.push(format!("{sign}{}", rel.to_string_lossy()));
    }
    Ok(ScopeConfig {
        directories,
        targets: cfg.targets.clone(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    includes: Vec<PathBuf>,
    excludes: Vec<PathBuf>,
    targets: BTreeSet<String>,
}

impl Scope {
    pub fn whole_repo() -> Self {
        Self::folder(Path::new(""))
    }

    pub fn folder(rel: &Path) -> Self {
        Scope {
            includes: vec![normalize(rel)],
            excludes: vec![],
            targets: BTreeSet::new(),
        }
    }

    pub fn from_config(
        cfg: &ScopeConfig,
        root_mapping: &BTreeMap<String, String>,
    ) -> anyhow::Result<Self> {
        let mut includes = Vec::new();
        let mut excludes = Vec::new();
        for dir in &cfg.directories {
            match dir.strip_prefix('-') {
                Some(excluded) => {
                    excludes.push(normalize(Path::new(excluded.trim_end_matches('/'))))
                }
                None => includes.push(normalize(Path::new(dir.trim_end_matches('/')))),
            }
        }
        if includes.is_empty() {
            includes.push(PathBuf::new());
        }
        let targets = cfg
            .targets
            .iter()
            .map(|t| canonical_label(t, root_mapping))
            .collect::<anyhow::Result<_>>()?;
        Ok(Scope {
            includes,
            excludes,
            targets,
        })
    }

    /// Whether a target, whose package lives at `rel_dir` (relative to the workspace root), is a source module.
    pub fn contains(&self, label: &str, rel_dir: &Path) -> bool {
        self.targets.contains(label) || self.contains_dir(rel_dir)
    }

    fn contains_dir(&self, rel_dir: &Path) -> bool {
        let deepest = |dirs: &[PathBuf]| {
            dirs.iter()
                .filter(|d| rel_dir.starts_with(d))
                .map(|d| d.components().count())
                .max()
        };
        match (deepest(&self.includes), deepest(&self.excludes)) {
            (Some(i), Some(e)) => i > e,
            (Some(_), None) => true,
            _ => false,
        }
    }
}

/// `//x:y` -> `@@//x:y`, `//x` -> `@@//x:x`, `@app//x:y` -> `@@<canonical>//x:y`, `@app` -> `@@<canonical>//:app`.
fn canonical_label(label: &str, root_mapping: &BTreeMap<String, String>) -> anyhow::Result<String> {
    let (canonical, rest, repo_name) = if let Some(rest) = label.strip_prefix("@@") {
        let (repo, rest) = split_repo(rest);
        (repo.to_string(), rest, repo)
    } else if let Some(rest) = label.strip_prefix("//") {
        (String::new(), Some(rest), "")
    } else if let Some(rest) = label.strip_prefix('@') {
        let (apparent, rest) = split_repo(rest);
        let Some(canonical) = root_mapping.get(apparent) else {
            bail!("target {label:?}: repository @{apparent} is not visible from the root module");
        };
        (canonical.clone(), rest, apparent)
    } else {
        bail!("target {label:?} must be an absolute label (//pkg:name or @repo//pkg:name)");
    };
    let (package, name) = match rest {
        None => ("", repo_name),
        Some(rest) => match rest.split_once(':') {
            Some((package, name)) => (package, name),
            None => (rest, rest.rsplit('/').next().unwrap_or(rest)),
        },
    };
    let is_pattern = package == "..."
        || package.ends_with("/...")
        || matches!(name, "" | "all" | "*" | "all-targets");
    if is_pattern {
        bail!(
            "target {label:?} must be a single target label, not a pattern; use a directory to include a tree"
        );
    }
    Ok(format!("@@{canonical}//{package}:{name}"))
}

/// `repo//pkg:name` -> (`repo`, Some(`pkg:name`)); `repo` -> (`repo`, None).
fn split_repo(rest: &str) -> (&str, Option<&str>) {
    match rest.split_once("//") {
        Some((repo, rest)) => (repo, Some(rest)),
        None => (rest, None),
    }
}

/// Target patterns for `bazel build`: positive patterns, negative ones, the scope's explicit targets (after the
/// negatives, so they are never subtracted), then `extra`.
pub fn target_patterns(scope: &Scope, modules: &[LocalModule], extra: &[String]) -> Vec<String> {
    let nested = |m: &&LocalModule| !m.rel_path.starts_with("..");
    let module_of = |dir: &Path| {
        modules
            .iter()
            .filter(|m| nested(m) && dir.starts_with(&m.rel_path))
            .max_by_key(|m| m.rel_path.components().count())
    };
    let package = |rel: &Path| rel.to_string_lossy().replace('\\', "/");
    let pattern = |repo: &str, rel: &Path| match package(rel).as_str() {
        "" => format!("@@{repo}//..."),
        p => format!("@@{repo}//{p}/..."),
    };
    let main_pattern = |rel: &Path| pattern("", rel).trim_start_matches("@@").to_string();

    let mut main = BTreeSet::new();
    let mut in_modules = BTreeSet::new();
    let mut negatives = Vec::new();
    for dir in &scope.includes {
        match module_of(dir) {
            Some(m) => {
                in_modules.insert((
                    m.name.clone(),
                    pattern(&m.canonical, dir.strip_prefix(&m.rel_path).unwrap()),
                ));
            }
            None => {
                main.insert(main_pattern(dir));
            }
        }
    }
    for m in modules {
        if scope.includes.iter().any(|d| m.rel_path.starts_with(d)) {
            in_modules.insert((m.name.clone(), pattern(&m.canonical, Path::new(""))));
            let main_covers = scope
                .includes
                .iter()
                .any(|d| module_of(d).is_none() && m.rel_path.starts_with(d));
            if nested(&m) && main_covers {
                negatives.push(format!("-{}", main_pattern(&m.rel_path)));
            }
        }
    }
    for dir in &scope.excludes {
        match module_of(dir) {
            Some(m) => negatives.push(format!(
                "-{}",
                pattern(&m.canonical, dir.strip_prefix(&m.rel_path).unwrap())
            )),
            None => negatives.push(format!("-{}", main_pattern(dir))),
        }
    }

    let mut out: Vec<String> = main.into_iter().collect();
    out.extend(in_modules.into_iter().map(|(_, p)| p));
    out.extend(negatives);
    out.extend(scope.targets.iter().cloned());
    out.extend(extra.iter().cloned());
    let mut seen = BTreeSet::new();
    out.retain(|p| seen.insert(p.clone()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods() -> Vec<LocalModule> {
        ["greeting", "shapes"]
            .iter()
            .map(|n| LocalModule {
                name: n.to_string(),
                canonical: format!("{n}+"),
                rel_path: PathBuf::from(format!("modules/{n}")),
                abs_path: PathBuf::from(format!("/ws/modules/{n}")),
            })
            .collect()
    }

    fn cfg(dirs: &[&str], targets: &[&str]) -> ScopeConfig {
        ScopeConfig {
            directories: dirs.iter().map(|s| s.to_string()).collect(),
            targets: targets.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn mapping() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("".to_string(), "".to_string()),
            ("greeting".to_string(), "greeting+".to_string()),
            ("shapes".to_string(), "shapes+".to_string()),
        ])
    }

    #[test]
    fn nested_module_dirs_are_excluded_from_main_patterns() {
        let p = target_patterns(&Scope::whole_repo(), &mods(), &[]);
        assert_eq!(
            p,
            [
                "//...",
                "@@greeting+//...",
                "@@shapes+//...",
                "-//modules/greeting/...",
                "-//modules/shapes/..."
            ]
        );
    }

    #[test]
    fn include_inside_module_maps_to_module_package() {
        let s =
            Scope::from_config(&cfg(&["modules/shapes/shapes", "app"], &[]), &mapping()).unwrap();
        assert_eq!(
            target_patterns(&s, &mods(), &[]),
            ["//app/...", "@@shapes+//shapes/..."]
        );
    }

    #[test]
    fn excludes_follow_positives() {
        let s = Scope::from_config(&cfg(&["app", "-app/legacy"], &[]), &mapping()).unwrap();
        assert_eq!(
            target_patterns(
                &s,
                &mods(),
                &["@@rules_kotlin+//kotlin/compiler:kotlin-stdlib".into()]
            ),
            [
                "//app/...",
                "-//app/legacy/...",
                "@@rules_kotlin+//kotlin/compiler:kotlin-stdlib"
            ]
        );
    }

    #[test]
    fn exclude_inside_module_uses_module_label() {
        let s = Scope::from_config(
            &cfg(&["modules/greeting", "-modules/greeting/legacy"], &[]),
            &mapping(),
        )
        .unwrap();
        assert_eq!(
            target_patterns(&s, &mods(), &[]),
            ["@@greeting+//...", "-@@greeting+//legacy/..."]
        );
    }

    #[test]
    fn module_outside_root_is_part_of_whole_repo() {
        let far = LocalModule {
            name: "far".into(),
            canonical: "far+".into(),
            rel_path: "../far".into(),
            abs_path: "/far".into(),
        };
        let p = target_patterns(&Scope::whole_repo(), &[far], &[]);
        assert_eq!(p, ["//...", "@@far+//..."]);
    }

    #[test]
    fn explicit_targets_are_built_after_negatives() {
        let s = Scope::from_config(
            &cfg(&["app", "-app/legacy"], &["//app/legacy:keep"]),
            &mapping(),
        )
        .unwrap();
        assert_eq!(
            target_patterns(&s, &mods(), &["@@x//:stdlib".into()]),
            [
                "//app/...",
                "-//app/legacy/...",
                "@@//app/legacy:keep",
                "@@x//:stdlib"
            ]
        );
    }

    #[test]
    fn contains_respects_longest_match() {
        let s = Scope::from_config(
            &cfg(&["app", "-app/legacy", "app/legacy/keep"], &[]),
            &mapping(),
        )
        .unwrap();
        assert!(s.contains("@@//app:a", Path::new("app")));
        assert!(s.contains("@@//app/x:a", Path::new("app/x")));
        assert!(!s.contains("@@//app/legacy:a", Path::new("app/legacy")));
        assert!(s.contains("@@//app/legacy/keep:a", Path::new("app/legacy/keep")));
        assert!(!s.contains("@@//other:a", Path::new("other")));
        assert!(!s.contains("@@//application:a", Path::new("application")));
    }

    #[test]
    fn explicit_target_is_in_scope() {
        let s = Scope::from_config(
            &cfg(
                &["app"],
                &["//tools:gen", "@greeting//:greeting", "@shapes"],
            ),
            &mapping(),
        )
        .unwrap();
        assert!(s.contains("@@//tools:gen", Path::new("tools")));
        assert!(s.contains("@@greeting+//:greeting", Path::new("modules/greeting")));
        assert!(s.contains("@@shapes+//:shapes", Path::new("modules/shapes")));
        assert!(!s.contains("@@//tools:other", Path::new("tools")));
    }

    #[test]
    fn shorthand_labels_get_their_implicit_name() {
        let s = Scope::from_config(
            &cfg(
                &["app"],
                &["//tools/gen", "@shapes//shapes", "@@greeting+//lib"],
            ),
            &mapping(),
        )
        .unwrap();
        assert!(s.contains("@@//tools/gen:gen", Path::new("tools/gen")));
        assert!(s.contains(
            "@@shapes+//shapes:shapes",
            Path::new("modules/shapes/shapes")
        ));
        assert!(s.contains("@@greeting+//lib:lib", Path::new("modules/greeting/lib")));
        assert_eq!(
            target_patterns(&s, &mods(), &[]),
            [
                "//app/...",
                "@@//tools/gen:gen",
                "@@greeting+//lib:lib",
                "@@shapes+//shapes:shapes"
            ]
        );
    }

    #[test]
    fn target_patterns_are_rejected() {
        for pattern in [
            "//app/...",
            "//app:all",
            "//app:*",
            "@greeting//...",
            "//app:all-targets",
            "//",
        ] {
            let err = Scope::from_config(&cfg(&[], &[pattern]), &mapping()).unwrap_err();
            assert!(
                err.to_string().contains("must be a single target label"),
                "{pattern}: {err}"
            );
        }
    }

    #[test]
    fn targets_or_excludes_alone_keep_the_whole_repo() {
        let only_targets = Scope::from_config(&cfg(&[], &["//tools:gen"]), &mapping()).unwrap();
        assert!(only_targets.contains("@@//app:a", Path::new("app")));
        assert!(only_targets.contains("@@//tools:gen", Path::new("tools")));
        let only_excludes = Scope::from_config(&cfg(&["-legacy"], &[]), &mapping()).unwrap();
        assert!(only_excludes.contains("@@//app:a", Path::new("app")));
        assert!(!only_excludes.contains("@@//legacy:a", Path::new("legacy")));
    }

    #[test]
    fn whole_repo_contains_everything() {
        assert!(Scope::whole_repo().contains("@@//a:b", Path::new("a")));
        assert!(Scope::folder(Path::new("app")).contains("@@//app/x:y", Path::new("app/x")));
        assert!(!Scope::folder(Path::new("app")).contains("@@//lib:y", Path::new("lib")));
    }

    #[test]
    fn directories_are_made_root_relative_and_must_exist() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::create_dir_all(root.join("app/legacy")).unwrap();
        let abs = |p: &str| root.join(p).display().to_string();
        let c = resolve_directories(
            &cfg(
                &[
                    &abs("app"),
                    &format!("-{}", abs("app/legacy")),
                    ".",
                    "-gone",
                ],
                &[],
            ),
            root,
        )
        .unwrap();
        assert_eq!(c.directories, ["app", "-app/legacy", "", "-gone"]);

        let err = format!(
            "{:#}",
            resolve_directories(&cfg(&["nope"], &[]), root).unwrap_err()
        );
        assert!(
            err.contains("nope") && err.contains(&root.display().to_string()),
            "{err}"
        );
        let err = format!(
            "{:#}",
            resolve_directories(&cfg(&["/elsewhere/app"], &[]), root).unwrap_err()
        );
        assert!(
            err.contains("/elsewhere/app") && err.contains("outside"),
            "{err}"
        );
    }

    #[test]
    fn missing_config_is_none() {
        let t = tempfile::tempdir().unwrap();
        assert!(
            load_scope_config(&t.path().join(".bazel-kotlin.toml"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn config_parses_toml() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join(".bazel-kotlin.toml");
        std::fs::write(
            &path,
            "directories = [\"app\", \"-app/legacy\"]\ntargets = [\"//tools:gen\"]\n",
        )
        .unwrap();
        let c = load_scope_config(&path).unwrap().unwrap();
        assert_eq!(c.directories, ["app", "-app/legacy"]);
        assert_eq!(c.targets, ["//tools:gen"]);
    }

    #[test]
    fn bad_toml_is_an_error_naming_the_file() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join(".bazel-kotlin.toml");
        std::fs::write(&path, "directories = app\n").unwrap();
        assert!(
            format!("{:#}", load_scope_config(&path).unwrap_err()).contains(".bazel-kotlin.toml")
        );
    }
}
