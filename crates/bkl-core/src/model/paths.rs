use std::path::{Path, PathBuf};

use super::ModelContext;

/// Maps an execroot-relative path from the aspect to a real path, without following symlinks.
pub fn resolve_exec_path(ctx: &ModelContext, p: &str) -> PathBuf {
    let path = Path::new(p);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    if let Ok(rest) = path.strip_prefix("external") {
        let mut parts = rest.components();
        if let Some(repo) = parts.next() {
            let repo = repo.as_os_str().to_string_lossy();
            return match ctx.local_module(&repo) {
                Some(m) => m.abs_path.join(parts.as_path()),
                None => ctx.output_base.join(path),
            };
        }
    }
    if path.starts_with("bazel-out") {
        return ctx.execution_root.join(path);
    }
    ctx.workspace_root.join(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::testing::ctx;

    #[test]
    fn exec_paths_resolve() {
        let c = ctx();
        assert_eq!(
            resolve_exec_path(&c, "app/src/A.kt"),
            PathBuf::from("/ws/app/src/A.kt")
        );
        assert_eq!(
            resolve_exec_path(&c, "bazel-out/k8-fastbuild/bin/app/B.kt"),
            PathBuf::from("/ob/execroot/_main/bazel-out/k8-fastbuild/bin/app/B.kt")
        );
        assert_eq!(
            resolve_exec_path(&c, "external/greeting+/src/G.kt"),
            PathBuf::from("/ws/modules/greeting/src/G.kt")
        );
        assert_eq!(
            resolve_exec_path(&c, "external/rules_jvm_external++maven+maven/x.jar"),
            PathBuf::from("/ob/external/rules_jvm_external++maven+maven/x.jar")
        );
        assert_eq!(resolve_exec_path(&c, "/abs/jdk"), PathBuf::from("/abs/jdk"));
    }
}
