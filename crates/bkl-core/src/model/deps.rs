use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use super::{Fs, ModelContext, ModuleGroup, Role, classify, resolve_exec_path};
use crate::aspect::TargetInfo;
use crate::workspace_json::{Library, LibraryRoot};

const STDLIB_KEY: &str = "org.jetbrains.kotlin:kotlin-stdlib";

/// Targets by label, with their roles.
pub(crate) struct Graph<'a> {
    pub targets: BTreeMap<&'a str, &'a TargetInfo>,
    pub roles: BTreeMap<&'a str, Role>,
}

impl<'a> Graph<'a> {
    pub fn new(targets: &'a [TargetInfo], ctx: &ModelContext) -> Self {
        Graph {
            targets: targets.iter().map(|t| (t.label.as_str(), t)).collect(),
            roles: targets
                .iter()
                .map(|t| (t.label.as_str(), classify(t, ctx)))
                .collect(),
        }
    }

    fn edges(&self, label: &str) -> impl Iterator<Item = &'a str> + '_ {
        let target = self.targets.get(label).copied();
        target
            .into_iter()
            .flat_map(|t| t.deps.iter().chain(&t.exports).chain(&t.associates))
            .filter_map(|d| self.targets.get_key_value(d.as_str()).map(|(k, _)| *k))
    }

    /// Everything reachable over deps, exports and associates; rules_kotlin compiles against all of it.
    pub fn closure(&self, from: &[String]) -> BTreeSet<String> {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut stack: Vec<&str> = from.iter().map(String::as_str).collect();
        while let Some(current) = stack.pop() {
            for dep in self.edges(current) {
                if seen.insert(dep) {
                    stack.push(dep);
                }
            }
        }
        seen.into_iter()
            .filter(|l| !from.iter().any(|f| f == l))
            .map(str::to_string)
            .collect()
    }

    /// The first modules and libraries reached when walking only through aggregators.
    pub fn direct(&self, from: &[String]) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut stack: Vec<&str> = from.iter().map(String::as_str).collect();
        while let Some(current) = stack.pop() {
            for dep in self.edges(current) {
                if !seen.insert(dep) || from.iter().any(|f| f == dep) {
                    continue;
                }
                match self.roles.get(dep) {
                    Some(Role::Module | Role::Library) => {
                        out.insert(dep.to_string());
                    }
                    Some(Role::Aggregator) => stack.push(dep),
                    _ => {}
                }
            }
        }
        out
    }
}

/// (`group:artifact[:packaging[:classifier]]`, version) from a `maven_coordinates=` tag.
pub(crate) fn maven_coordinate(t: &TargetInfo) -> Option<(String, String)> {
    let coordinate = t
        .tags
        .iter()
        .find_map(|tag| tag.strip_prefix("maven_coordinates="))?;
    let (key, version) = coordinate.rsplit_once(':')?;
    Some((key.to_string(), version.to_string()))
}

pub(crate) fn compare_versions(a: &str, b: &str) -> Ordering {
    let split = |v: &str| v.split(['.', '-']).map(str::to_string).collect::<Vec<_>>();
    let (a, b) = (split(a), split(b));
    for (x, y) in a.iter().zip(&b) {
        let ordering = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            _ => x.cmp(y),
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    a.len().cmp(&b.len())
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ModuleDeps {
    pub modules: Vec<String>,
    pub libraries: Vec<String>,
}

pub(crate) fn module_deps(
    group: &ModuleGroup,
    graph: &Graph,
    names: &BTreeMap<String, String>,
    ctx: &ModelContext,
) -> ModuleDeps {
    let closure = graph.closure(&group.labels);
    let direct = graph.direct(&group.labels);
    let role = |l: &str| graph.roles.get(l).copied();

    let modules: BTreeSet<String> = closure
        .iter()
        .filter(|l| role(l) == Some(Role::Module))
        .filter_map(|l| names.get(l).cloned())
        .filter(|n| *n != group.name)
        .collect();

    let stdlib = ctx
        .kotlin_stdlib
        .as_deref()
        .filter(|s| graph.targets.contains_key(s));
    let mut chosen: BTreeSet<String> = BTreeSet::new();
    let mut by_key: BTreeMap<String, Vec<(&str, String)>> = BTreeMap::new();
    for label in closure.iter().filter(|l| role(l) == Some(Role::Library)) {
        match maven_coordinate(graph.targets[label.as_str()]) {
            Some((key, _)) if stdlib.is_some() && key == STDLIB_KEY => {}
            Some((key, version)) => by_key.entry(key).or_default().push((label, version)),
            None => {
                chosen.insert(label.clone());
            }
        }
    }
    for candidates in by_key.values() {
        let direct_ones: Vec<&(&str, String)> = candidates
            .iter()
            .filter(|(l, _)| direct.contains(*l))
            .collect();
        let pool: Vec<&(&str, String)> = if direct_ones.is_empty() {
            candidates.iter().collect()
        } else {
            direct_ones
        };
        if let Some((label, _)) = pool
            .into_iter()
            .max_by(|a, b| compare_versions(&a.1, &b.1).then(b.0.cmp(a.0)))
        {
            chosen.insert(label.to_string());
        }
    }
    if let Some(stdlib) = stdlib {
        chosen.insert(stdlib.to_string());
    }
    ModuleDeps {
        modules: modules.into_iter().collect(),
        libraries: chosen.into_iter().collect(),
    }
}

/// Labels of libraries whose jars Bazel must build (everything but imported jars).
pub fn jar_targets(targets: &[TargetInfo], ctx: &ModelContext) -> Vec<String> {
    targets
        .iter()
        .filter(|t| t.import_jars.is_empty() && classify(t, ctx) == Role::Library)
        .map(|t| t.label.clone())
        .collect()
}

pub(crate) fn library_entry(
    t: &TargetInfo,
    ctx: &ModelContext,
    fs: &dyn Fs,
    warnings: &mut Vec<String>,
) -> Option<Library> {
    let classes = if t.import_jars.is_empty() {
        &t.class_jars
    } else {
        &t.import_jars
    };
    let sources = if t.import_srcjar.is_empty() {
        &t.source_jars
    } else {
        &t.import_srcjar
    };
    let mut existing = |paths: &[String], kind: Option<&str>| -> Vec<LibraryRoot> {
        let mut roots = Vec::new();
        for p in paths {
            let path = resolve_exec_path(ctx, p);
            if fs.exists(&path) {
                roots.push(LibraryRoot {
                    path: path.to_string_lossy().into_owned(),
                    root_type: kind.map(str::to_string),
                });
            } else {
                warnings.push(format!("{}: missing {}", t.label, path.display()));
            }
        }
        roots
    };
    let mut roots = existing(classes, None);
    if roots.is_empty() {
        if classes.is_empty() {
            warnings.push(format!("{}: no jar", t.label));
        }
        return None;
    }
    roots.extend(existing(sources, Some("SOURCES")));
    Some(Library {
        name: t.label.clone(),
        library_type: "COMPILE".into(),
        roots,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::testing::{FakeFs, ctx, target};
    use crate::model::{ModuleGroup, group_modules};

    fn maven(name: &str, coordinate: &str, deps: &[&str]) -> TargetInfo {
        let mut t = target(&format!("@@maven//:{name}"), "jvm_import");
        t.tags = vec![format!("maven_coordinates={coordinate}")];
        t.import_jars = vec![format!("external/maven/{name}.jar")];
        t.import_srcjar = vec![format!("external/maven/{name}-sources.jar")];
        t.deps = deps.iter().map(|d| format!("@@maven//:{d}")).collect();
        t
    }

    fn module(label: &str, deps: &[&str]) -> TargetInfo {
        let mut t = target(label, "kt_jvm_library");
        t.srcs = vec![format!("{}/A.kt", t.package)];
        t.deps = deps.iter().map(|d| d.to_string()).collect();
        t
    }

    struct World {
        targets: Vec<TargetInfo>,
        ctx: crate::model::ModelContext,
    }

    impl World {
        fn deps_of(&self, label: &str) -> ModuleDeps {
            let fs = FakeFs::default();
            let (groups, names) = group_modules(&self.targets, &self.ctx, &fs);
            let g: &ModuleGroup = groups
                .iter()
                .find(|g| g.labels.iter().any(|l| l == label))
                .unwrap();
            let graph = Graph::new(&self.targets, &self.ctx);
            module_deps(g, &graph, &names, &self.ctx)
        }
    }

    #[test]
    fn closure_walks_through_aggregator() {
        let mut agg = target("@@//deps:serialization", "java_library");
        agg.exports = vec!["@@maven//:ser".into()];
        let targets = vec![
            module("@@//app:app", &["@@//deps:serialization"]),
            agg,
            maven("ser", "g:ser:1.0", &[]),
        ];
        let graph = Graph::new(&targets, &ctx());
        let from = vec!["@@//app:app".to_string()];
        assert!(graph.closure(&from).contains("@@maven//:ser"));
        assert_eq!(
            graph.direct(&from),
            BTreeSet::from(["@@maven//:ser".to_string()])
        );
    }

    #[test]
    fn dedupe_prefers_direct_version() {
        let w = World {
            targets: vec![
                module("@@//app:app", &["@@maven//:a_1", "@@maven//:b"]),
                maven("a_1", "g:a:1.0", &[]),
                maven("b", "g:b:1.0", &["a_2"]),
                maven("a_2", "g:a:2.0", &[]),
            ],
            ctx: ctx(),
        };
        assert_eq!(
            w.deps_of("@@//app:app").libraries,
            ["@@maven//:a_1", "@@maven//:b"]
        );
    }

    #[test]
    fn dedupe_highest_when_none_direct() {
        let w = World {
            targets: vec![
                module("@@//app:app", &["@@maven//:b", "@@maven//:c"]),
                maven("b", "g:b:1.0", &["a_old"]),
                maven("c", "g:c:1.0", &["a_new"]),
                maven("a_old", "g:a:2.9.1", &[]),
                maven("a_new", "g:a:2.10.0", &[]),
            ],
            ctx: ctx(),
        };
        assert_eq!(
            w.deps_of("@@//app:app").libraries,
            ["@@maven//:a_new", "@@maven//:b", "@@maven//:c"]
        );
    }

    #[test]
    fn stdlib_replaces_maven_stdlib() {
        let mut stdlib = target(
            "@@rules_kotlin+//kotlin/compiler:kotlin-stdlib",
            "kt_jvm_import",
        );
        stdlib.import_jars = vec!["external/rules_kotlin+/stdlib.jar".into()];
        let mut c = ctx();
        c.kotlin_stdlib = Some(stdlib.label.clone());
        let w = World {
            targets: vec![
                module("@@//app:app", &["@@maven//:ser"]),
                maven("ser", "g:ser:1.0", &["std"]),
                maven("std", "org.jetbrains.kotlin:kotlin-stdlib:2.2.0", &[]),
                stdlib,
            ],
            ctx: c,
        };
        assert_eq!(
            w.deps_of("@@//app:app").libraries,
            [
                "@@maven//:ser",
                "@@rules_kotlin+//kotlin/compiler:kotlin-stdlib"
            ]
        );
    }

    #[test]
    fn merged_group_has_no_self_dep() {
        let mut lib = module("@@shapes+//shapes:shapes", &[]);
        lib.srcs = vec!["external/shapes+/shapes/Shape.kt".into()];
        let mut test = target("@@shapes+//shapes:shapes_test", "kt_jvm_test");
        test.srcs = vec!["external/shapes+/shapes/ShapeTest.kt".into()];
        test.associates = vec!["@@shapes+//shapes:shapes".into()];
        let w = World {
            targets: vec![
                lib,
                test,
                module("@@//app:app", &["@@shapes+//shapes:shapes"]),
            ],
            ctx: ctx(),
        };
        assert!(w.deps_of("@@shapes+//shapes:shapes").modules.is_empty());
        assert_eq!(w.deps_of("@@//app:app").modules, ["shapes.shapes.shapes"]);
    }

    #[test]
    fn version_ordering() {
        assert_eq!(compare_versions("2.10.0", "2.9.1"), Ordering::Greater);
        assert_eq!(compare_versions("1.0", "1.0.1"), Ordering::Less);
        assert_eq!(compare_versions("1.0-rc1", "1.0-rc2"), Ordering::Less);
        assert_eq!(compare_versions("33.0-jre", "33.0-jre"), Ordering::Equal);
    }

    #[test]
    fn coordinates_drop_the_version() {
        let t = maven("x", "g:a:jar:sources:1.2", &[]);
        assert_eq!(
            maven_coordinate(&t),
            Some(("g:a:jar:sources".into(), "1.2".into()))
        );
        assert_eq!(maven_coordinate(&target("@@//a:a", "java_library")), None);
    }

    #[test]
    fn jar_targets_lists_built_libraries_only() {
        let mut version = target("@@//gen:version", "kt_jvm_library");
        version.generated_srcs = vec!["bazel-out/k8/bin/gen/Version.kt".into()];
        version.class_jars = vec!["bazel-out/k8/bin/gen/version.jar".into()];
        let targets = vec![
            version,
            maven("ser", "g:ser:1.0", &[]),
            module("@@//app:app", &[]),
        ];
        assert_eq!(jar_targets(&targets, &ctx()), ["@@//gen:version"]);
    }

    #[test]
    fn missing_classes_root_drops_library_with_warning() {
        let t = maven("gone", "g:gone:1", &[]);
        let mut fs = FakeFs::default();
        fs.missing.insert("/ob/external/maven/gone.jar".into());
        let mut warnings = Vec::new();
        assert!(library_entry(&t, &ctx(), &fs, &mut warnings).is_none());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("@@maven//:gone"));
    }

    #[test]
    fn library_entry_lists_classes_then_sources() {
        let t = maven("ser", "g:ser:1.0", &[]);
        let mut warnings = Vec::new();
        let lib = library_entry(&t, &ctx(), &FakeFs::default(), &mut warnings).unwrap();
        assert_eq!(lib.name, "@@maven//:ser");
        assert_eq!(lib.roots[0].path, "/ob/external/maven/ser.jar");
        assert_eq!(lib.roots[0].root_type, None);
        assert_eq!(lib.roots[1].root_type.as_deref(), Some("SOURCES"));
        assert!(warnings.is_empty());
    }
}
