//! Builds kotlin-lsp's project model from the aspect's targets.

#[allow(dead_code)] // used by build_model (Task 8)
mod modules;
mod paths;
mod roles;
mod sources;

use std::path::{Path, PathBuf};

use crate::bazel_mod::LocalModule;
use crate::scope::Scope;

#[allow(unused_imports)]
pub(crate) use modules::{ModuleGroup, SourceRootEntry, group_modules};
pub use paths::resolve_exec_path;
pub use roles::{Role, classify, module_name};
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
