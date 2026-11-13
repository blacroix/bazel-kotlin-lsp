use std::path::{Path, PathBuf};

use super::ModelContext;
use crate::aspect::TargetInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// A first-party target with sources, in scope: edited as a module.
    Module,
    /// Jars on the classpath: Maven imports, generated-only targets, out-of-scope or external targets.
    Library,
    /// A JVM target with neither sources nor jars, such as an `exports`-only `java_library`; walked through.
    Aggregator,
    Ignored,
}

pub fn classify(t: &TargetInfo, ctx: &ModelContext) -> Role {
    if t.kind.contains("android") {
        return Role::Ignored;
    }
    let first_party = t.repo.is_empty() || ctx.local_module(&t.repo).is_some();
    if t.repo.is_empty() && inside_local_module(Path::new(&t.package), ctx) {
        return Role::Ignored;
    }
    if first_party && !t.srcs.is_empty() {
        let in_scope = rel_dir(t, ctx).is_some_and(|dir| ctx.scope.contains(&t.label, &dir));
        if in_scope {
            return Role::Module;
        }
    }
    if !t.import_jars.is_empty() || !t.class_jars.is_empty() {
        return Role::Library;
    }
    Role::Aggregator
}

fn inside_local_module(package: &Path, ctx: &ModelContext) -> bool {
    ctx.local_modules
        .iter()
        .any(|m| !m.rel_path.starts_with("..") && package.starts_with(&m.rel_path))
}

/// The package folder of a first-party target, relative to the workspace root.
pub(crate) fn rel_dir(t: &TargetInfo, ctx: &ModelContext) -> Option<PathBuf> {
    if t.repo.is_empty() {
        return Some(PathBuf::from(&t.package));
    }
    ctx.local_module(&t.repo)
        .map(|m| crate::bazel_mod::normalize(&m.rel_path.join(&t.package)))
}

/// `<module>.<package with / as .>.<target>`, e.g. `fixture.app.app` for `@@//app:app` in module `fixture`.
pub fn module_name(label: &str, ctx: &ModelContext) -> String {
    let rest = label.trim_start_matches('@');
    let (repo, rest) = rest.split_once("//").unwrap_or(("", rest));
    let (package, name) = rest
        .split_once(':')
        .unwrap_or((rest, rest.rsplit('/').next().unwrap_or(rest)));
    let module = if repo.is_empty() {
        ctx.root_module_name.clone()
    } else {
        ctx.local_module(repo)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| repo.trim_end_matches('+').to_string())
    };
    [module.as_str(), &package.replace('/', "."), name]
        .iter()
        .filter(|p| !p.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(".")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::testing::{ctx, target};
    use crate::scope::Scope;

    fn with_srcs(mut t: TargetInfo) -> TargetInfo {
        t.srcs = vec!["x/A.kt".into()];
        t
    }

    #[test]
    fn first_party_with_sources_is_module() {
        assert_eq!(
            classify(&with_srcs(target("@@//app:app", "kt_jvm_library")), &ctx()),
            Role::Module
        );
        assert_eq!(
            classify(
                &with_srcs(target("@@greeting+//:greeting", "kt_jvm_library")),
                &ctx()
            ),
            Role::Module
        );
    }

    #[test]
    fn main_repo_target_inside_local_module_is_ignored() {
        let t = with_srcs(target("@@//modules/greeting:greeting", "kt_jvm_library"));
        assert_eq!(classify(&t, &ctx()), Role::Ignored);
    }

    #[test]
    fn out_of_scope_first_party_is_library() {
        let mut c = ctx();
        c.scope = Scope::folder(Path::new("app"));
        let mut t = with_srcs(target("@@greeting+//:greeting", "kt_jvm_library"));
        t.class_jars = vec!["bazel-out/x/bin/external/greeting+/greeting.jar".into()];
        assert_eq!(classify(&t, &c), Role::Library);
    }

    #[test]
    fn generated_only_target_is_library() {
        let mut t = target("@@//gen:version", "kt_jvm_library");
        t.generated_srcs = vec!["bazel-out/x/bin/gen/Version.kt".into()];
        t.class_jars = vec!["bazel-out/x/bin/gen/version.jar".into()];
        assert_eq!(classify(&t, &ctx()), Role::Library);
    }

    #[test]
    fn import_is_library_and_jarless_is_aggregator() {
        let mut maven = target(
            "@@rules_jvm_external++maven+maven//:junit_junit",
            "jvm_import",
        );
        maven.import_jars = vec!["external/x/junit.jar".into()];
        assert_eq!(classify(&maven, &ctx()), Role::Library);
        assert_eq!(
            classify(&target("@@//deps:serialization", "java_library"), &ctx()),
            Role::Aggregator
        );
    }

    #[test]
    fn android_kind_is_ignored() {
        assert_eq!(
            classify(
                &with_srcs(target("@@//ui:ui", "kt_android_library")),
                &ctx()
            ),
            Role::Ignored
        );
    }

    #[test]
    fn module_names() {
        let c = ctx();
        assert_eq!(module_name("@@//app:app", &c), "fixture.app.app");
        assert_eq!(
            module_name("@@greeting+//:greeting", &c),
            "greeting.greeting"
        );
        assert_eq!(
            module_name("@@shapes+//shapes:shapes_test", &c),
            "shapes.shapes.shapes_test"
        );
        assert_eq!(module_name("@@other+//a/b:c", &c), "other.a.b.c");
    }
}
