//! Builds kotlin-lsp's project model from the aspect's targets.

mod deps;
mod modules;
mod paths;
mod roles;
mod settings;
mod sources;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::aspect::TargetInfo;
use crate::bazel_mod::LocalModule;
use crate::scope::Scope;
use crate::workspace_json::{ContentRoot, Dependency, Module, SourceRootJson, Workspace};

pub use crate::workspace_json::{PLACEHOLDER, render};
pub use deps::jar_targets;
pub(crate) use modules::{ModuleGroup, group_modules};
pub use paths::resolve_exec_path;
pub use roles::{Role, classify, module_name};
pub use settings::java_major;
pub use sources::{SourceRoot, kotlin_package, source_root};

/// Everything about the workspace that the model needs besides the aspect's targets.
#[derive(Debug, Clone)]
pub struct ModelContext {
    pub workspace_root: PathBuf,
    pub execution_root: PathBuf,
    pub output_base: PathBuf,
    pub root_module_name: String,
    pub local_modules: Vec<LocalModule>,
    pub scope: Scope,
    /// Label of the Kotlin toolchain's stdlib, added to every module.
    pub kotlin_stdlib: Option<String>,
    pub java_home_fallback: Option<PathBuf>,
}

impl ModelContext {
    pub(crate) fn local_module(&self, canonical: &str) -> Option<&LocalModule> {
        self.local_modules.iter().find(|m| m.canonical == canonical)
    }
}

pub struct Model {
    pub workspace: Workspace,
    pub warnings: Vec<String>,
}

/// The `workspace.json` model for the aspect's targets. Deterministic: everything is sorted.
pub fn build_model(targets: &[TargetInfo], ctx: &ModelContext, fs: &dyn Fs) -> Model {
    let mut warnings = Vec::new();
    let graph = deps::Graph::new(targets, ctx);
    let (groups, names) = group_modules(targets, ctx, fs);
    let sdk = settings::sdk(targets, ctx, fs, &mut warnings);

    let group_deps: Vec<deps::ModuleDeps> = groups
        .iter()
        .map(|g| deps::module_deps(g, &graph, &names, ctx))
        .collect();
    let used: BTreeSet<&str> = group_deps
        .iter()
        .flat_map(|d| d.libraries.iter().map(String::as_str))
        .collect();
    let libraries: BTreeMap<&str, crate::workspace_json::Library> = used
        .into_iter()
        .filter_map(|label| {
            deps::library_entry(graph.targets[label], ctx, fs, &mut warnings).map(|l| (label, l))
        })
        .collect();

    let mut modules = Vec::new();
    let mut kotlin_settings = Vec::new();
    for (group, module_deps) in groups.iter().zip(&group_deps) {
        let mut dependencies: Vec<Dependency> = module_deps
            .modules
            .iter()
            .map(|name| Dependency::Module {
                name: name.clone(),
                scope: "compile".into(),
            })
            .collect();
        dependencies.extend(
            module_deps
                .libraries
                .iter()
                .filter(|l| libraries.contains_key(l.as_str()))
                .map(|l| Dependency::Library {
                    name: l.clone(),
                    scope: "compile".into(),
                }),
        );
        dependencies.push(Dependency::ModuleSource);
        if let Some(sdk) = &sdk {
            dependencies.push(Dependency::Sdk {
                name: sdk.name.clone(),
                kind: "JavaSDK".into(),
            });
        }
        let content_roots = group
            .roots
            .iter()
            .map(|(path, entry)| {
                let path = path.to_string_lossy().into_owned();
                ContentRoot {
                    path: path.clone(),
                    source_roots: vec![SourceRootJson {
                        path,
                        root_type: if entry.test {
                            "java-test"
                        } else {
                            "java-source"
                        }
                        .into(),
                        package_prefix: entry.package_prefix.clone(),
                        generated: entry.generated,
                    }],
                }
            })
            .collect();
        modules.push(Module {
            name: group.name.clone(),
            module_type: "JAVA_MODULE".into(),
            dependencies,
            content_roots,
        });

        let members: Vec<&TargetInfo> = group
            .labels
            .iter()
            .map(|l| graph.targets[l.as_str()])
            .collect();
        let associates: BTreeSet<String> = members
            .iter()
            .flat_map(|t| &t.associates)
            .filter_map(|a| names.get(a))
            .filter(|n| **n != group.name)
            .cloned()
            .collect();
        let args = settings::compiler_arguments(&members, ctx);
        kotlin_settings.push(settings::kotlin_settings(
            group,
            associates.into_iter().collect(),
            args,
        ));
    }

    Model {
        workspace: Workspace {
            modules,
            libraries: libraries.into_values().collect(),
            sdks: sdk.into_iter().collect(),
            kotlin_settings,
            java_settings: vec![],
        },
        warnings,
    }
}

/// The filesystem reads the model makes, so tests can run without a real tree.
pub trait Fs {
    fn exists(&self, p: &Path) -> bool;
    /// The first 8 KiB of a text file.
    fn read_head(&self, p: &Path) -> Option<String>;
}

pub struct RealFs;

impl Fs for RealFs {
    fn exists(&self, p: &Path) -> bool {
        p.exists()
    }

    fn read_head(&self, p: &Path) -> Option<String> {
        use std::io::Read;
        let mut buf = vec![0; 8192];
        let n = std::fs::File::open(p).ok()?.read(&mut buf).ok()?;
        buf.truncate(n);
        Some(String::from_utf8_lossy(&buf).into_owned())
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::aspect::TargetInfo;
    use crate::bazel_mod::LocalModule;
    use crate::scope::Scope;

    /// Every path exists unless listed in `missing`; heads come from `heads`.
    #[derive(Default)]
    pub struct FakeFs {
        pub heads: HashMap<PathBuf, String>,
        pub missing: HashSet<PathBuf>,
    }

    impl FakeFs {
        pub fn with(files: &[(&str, &str)]) -> Self {
            FakeFs {
                heads: files
                    .iter()
                    .map(|(p, h)| (PathBuf::from(p), h.to_string()))
                    .collect(),
                ..Default::default()
            }
        }
    }

    impl Fs for FakeFs {
        fn exists(&self, p: &Path) -> bool {
            !self.missing.contains(p)
        }
        fn read_head(&self, p: &Path) -> Option<String> {
            self.heads.get(p).cloned()
        }
    }

    pub fn ctx() -> ModelContext {
        ModelContext {
            workspace_root: "/ws".into(),
            execution_root: "/ob/execroot/_main".into(),
            output_base: "/ob".into(),
            root_module_name: "fixture".into(),
            local_modules: ["greeting", "shapes"]
                .iter()
                .map(|n| LocalModule {
                    name: n.to_string(),
                    canonical: format!("{n}+"),
                    rel_path: format!("modules/{n}").into(),
                    abs_path: format!("/ws/modules/{n}").into(),
                })
                .collect(),
            scope: Scope::whole_repo(),
            kotlin_stdlib: None,
            java_home_fallback: None,
        }
    }

    pub fn target(label: &str, kind: &str) -> TargetInfo {
        let rest = label.trim_start_matches("@@");
        let (repo, rest) = rest.split_once("//").unwrap();
        let (package, name) = rest.split_once(':').unwrap();
        TargetInfo {
            label: label.into(),
            repo: repo.into(),
            package: package.into(),
            name: name.into(),
            kind: kind.into(),
            ..Default::default()
        }
    }
}
