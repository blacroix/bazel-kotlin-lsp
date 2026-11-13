use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{
    Fs, ModelContext, Role, classify, kotlin_package, module_name, resolve_exec_path, source_root,
};
use crate::aspect::TargetInfo;

const TEST_KINDS: [&str; 2] = ["kt_jvm_test", "java_test"];
const SOURCE_EXTENSIONS: [&str; 2] = ["kt", "java"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceRootEntry {
    pub package_prefix: Option<String>,
    pub generated: bool,
    pub test: bool,
}

/// One kotlin-lsp module: one target, or several targets sharing a source root.
#[derive(Debug, Clone)]
pub(crate) struct ModuleGroup {
    pub name: String,
    pub labels: Vec<String>,
    pub roots: BTreeMap<PathBuf, SourceRootEntry>,
    pub is_test: bool,
}

/// Groups module targets into kotlin-lsp modules, merging targets whose source roots overlap. Returns the groups
/// sorted by name, and each member label's module name.
pub(crate) fn group_modules(
    targets: &[TargetInfo],
    ctx: &ModelContext,
    fs: &dyn Fs,
) -> (Vec<ModuleGroup>, BTreeMap<String, String>) {
    let members: Vec<&TargetInfo> = targets
        .iter()
        .filter(|t| classify(t, ctx) == Role::Module)
        .collect();
    let roots: Vec<Vec<(PathBuf, SourceRootEntry)>> =
        members.iter().map(|t| target_roots(t, ctx, fs)).collect();

    let mut parent: Vec<usize> = (0..members.len()).collect();
    fn find(parent: &mut [usize], i: usize) -> usize {
        let mut i = i;
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let mut owner: BTreeMap<&Path, usize> = BTreeMap::new();
    for (i, target_roots) in roots.iter().enumerate() {
        for (path, _) in target_roots.iter().filter(|(_, e)| !e.generated) {
            match owner.get(path.as_path()) {
                Some(&j) => {
                    let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                    parent[a.max(b)] = a.min(b);
                }
                None => {
                    owner.insert(path, i);
                }
            }
        }
    }

    let mut by_root: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..members.len() {
        let r = find(&mut parent, i);
        by_root.entry(r).or_default().push(i);
    }
    let mut groups = Vec::new();
    let mut names = BTreeMap::new();
    for indices in by_root.values() {
        let mut labels: Vec<String> = indices.iter().map(|&i| members[i].label.clone()).collect();
        labels.sort();
        let name = module_name(&labels[0], ctx);
        let is_test = indices.iter().all(|&i| is_test(members[i]));
        let mut merged: BTreeMap<PathBuf, SourceRootEntry> = BTreeMap::new();
        for &i in indices {
            for (path, entry) in &roots[i] {
                merged
                    .entry(path.clone())
                    .and_modify(|e| e.test &= entry.test)
                    .or_insert_with(|| entry.clone());
            }
        }
        for label in &labels {
            names.insert(label.clone(), name.clone());
        }
        groups.push(ModuleGroup {
            name,
            labels,
            roots: merged,
            is_test,
        });
    }
    groups.sort_by(|a, b| a.name.cmp(&b.name));
    (groups, names)
}

fn is_test(t: &TargetInfo) -> bool {
    TEST_KINDS.contains(&t.kind.as_str())
}

fn target_roots(
    t: &TargetInfo,
    ctx: &ModelContext,
    fs: &dyn Fs,
) -> Vec<(PathBuf, SourceRootEntry)> {
    let test = is_test(t);
    let sources = t.srcs.iter().map(|s| (s, false));
    let generated = t
        .generated_srcs
        .iter()
        .filter(|s| {
            SOURCE_EXTENSIONS
                .iter()
                .any(|ext| s.ends_with(&format!(".{ext}")))
        })
        .map(|s| (s, true));
    let mut out: Vec<(PathBuf, SourceRootEntry)> = Vec::new();
    for (src, is_generated) in sources.chain(generated) {
        let file = resolve_exec_path(ctx, src);
        let package = fs.read_head(&file).and_then(|head| kotlin_package(&head));
        let root = source_root(&file, package.as_deref(), is_generated);
        if out.iter().all(|(p, _)| *p != root.path) {
            out.push((
                root.path,
                SourceRootEntry {
                    package_prefix: root.package_prefix,
                    generated: is_generated,
                    test,
                },
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::testing::{FakeFs, ctx, target};

    fn kt(label: &str, kind: &str, srcs: &[&str]) -> TargetInfo {
        let mut t = target(label, kind);
        t.srcs = srcs.iter().map(|s| s.to_string()).collect();
        t
    }

    #[test]
    fn lib_and_test_in_one_dir_merge() {
        let fs = FakeFs::with(&[
            (
                "/ws/modules/shapes/shapes/Shape.kt",
                "package fixture.shapes\n",
            ),
            (
                "/ws/modules/shapes/shapes/ShapeTest.kt",
                "package fixture.shapes\n",
            ),
        ]);
        let targets = vec![
            kt(
                "@@shapes+//shapes:shapes",
                "kt_jvm_library",
                &["external/shapes+/shapes/Shape.kt"],
            ),
            kt(
                "@@shapes+//shapes:shapes_test",
                "kt_jvm_test",
                &["external/shapes+/shapes/ShapeTest.kt"],
            ),
        ];
        let (groups, names) = group_modules(&targets, &ctx(), &fs);
        assert_eq!(groups.len(), 1);
        let g = &groups[0];
        assert_eq!(g.name, "shapes.shapes.shapes");
        assert!(!g.is_test);
        assert_eq!(
            names["@@shapes+//shapes:shapes_test"],
            "shapes.shapes.shapes"
        );
        let root = &g.roots[Path::new("/ws/modules/shapes/shapes")];
        assert!(!root.test);
        assert_eq!(root.package_prefix.as_deref(), Some("fixture.shapes"));
    }

    #[test]
    fn separate_roots_stay_separate_modules() {
        let fs = FakeFs::with(&[
            (
                "/ws/app/src/main/kotlin/fixture/app/App.kt",
                "package fixture.app\n",
            ),
            (
                "/ws/app/src/test/kotlin/fixture/app/AppTest.kt",
                "package fixture.app\n",
            ),
        ]);
        let targets = vec![
            kt(
                "@@//app:app",
                "kt_jvm_library",
                &["app/src/main/kotlin/fixture/app/App.kt"],
            ),
            kt(
                "@@//app:app_test",
                "kt_jvm_test",
                &["app/src/test/kotlin/fixture/app/AppTest.kt"],
            ),
        ];
        let (groups, _) = group_modules(&targets, &ctx(), &fs);
        let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["fixture.app.app", "fixture.app.app_test"]);
        assert!(groups[1].is_test);
        assert!(groups[1].roots[Path::new("/ws/app/src/test/kotlin")].test);
    }

    #[test]
    fn generated_sources_add_generated_root() {
        let fs = FakeFs::with(&[
            (
                "/ws/app/src/main/kotlin/fixture/app/App.kt",
                "package fixture.app\n",
            ),
            (
                "/ob/execroot/_main/bazel-out/k8-fastbuild/bin/app/BuildInfo.kt",
                "package fixture.app\n",
            ),
        ]);
        let mut t = kt(
            "@@//app:app",
            "kt_jvm_library",
            &["app/src/main/kotlin/fixture/app/App.kt"],
        );
        t.generated_srcs = vec![
            "bazel-out/k8-fastbuild/bin/app/BuildInfo.kt".into(),
            "bazel-out/k8-fastbuild/bin/app/x.srcjar".into(),
        ];
        let (groups, _) = group_modules(&[t], &ctx(), &fs);
        let gen_root =
            &groups[0].roots[Path::new("/ob/execroot/_main/bazel-out/k8-fastbuild/bin/app")];
        assert!(gen_root.generated);
        assert_eq!(gen_root.package_prefix.as_deref(), Some("fixture.app"));
        assert_eq!(groups[0].roots.len(), 2);
    }

    #[test]
    fn non_modules_are_not_grouped() {
        let fs = FakeFs::default();
        let (groups, names) = group_modules(
            &[target("@@//deps:serialization", "java_library")],
            &ctx(),
            &fs,
        );
        assert!(groups.is_empty() && names.is_empty());
    }
}
