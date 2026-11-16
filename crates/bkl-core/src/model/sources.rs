use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRoot {
    pub path: PathBuf,
    pub package_prefix: Option<String>,
    pub generated: bool,
}

/// The `package` of a Kotlin or Java file, from its first lines.
pub fn kotlin_package(head: &str) -> Option<String> {
    let mut text = head;
    loop {
        text = text.trim_start();
        if let Some(rest) = text.strip_prefix("/*") {
            text = rest.split_once("*/")?.1;
        } else if text.starts_with("//") || text.starts_with("#!") || text.starts_with("@file:") {
            text = text.split_once('\n').map(|(_, rest)| rest).unwrap_or("");
        } else {
            let rest = text.strip_prefix("package")?;
            if !rest.starts_with(char::is_whitespace) {
                return None;
            }
            let name: String = rest
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.' || *c == '`')
                .filter(|c| *c != '`')
                .collect();
            return (!name.is_empty()).then_some(name);
        }
    }
}

/// The source root of a file: its folder minus the package path when the folder ends with it, otherwise the
/// folder with the package as prefix.
pub fn source_root(file: &Path, package: Option<&str>, generated: bool) -> SourceRoot {
    let dir = file.parent().unwrap_or(Path::new("")).to_path_buf();
    let Some(package) = package.filter(|p| !p.is_empty()) else {
        return SourceRoot {
            path: dir,
            package_prefix: None,
            generated,
        };
    };
    let package_path: PathBuf = package.split('.').collect();
    let n = package_path.components().count();
    if dir.ends_with(&package_path) {
        let root: PathBuf = dir
            .components()
            .take(dir.components().count() - n)
            .collect();
        return SourceRoot {
            path: root,
            package_prefix: None,
            generated,
        };
    }
    SourceRoot {
        path: dir,
        package_prefix: Some(package.to_string()),
        generated,
    }
}

/// The root of a file whose Bazel package folder lies strictly below `root` (the folder minus the package path):
/// that package folder, with its package as prefix. Targets then share a root only when they share a folder.
pub fn package_folder_root(root: SourceRoot, file: &Path, package_dir: &Path) -> SourceRoot {
    if root.generated || root.package_prefix.is_some() || !file.starts_with(package_dir) {
        return root;
    }
    let rel = match package_dir.strip_prefix(&root.path) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel,
        _ => return root,
    };
    let prefix: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    SourceRoot {
        path: package_dir.to_path_buf(),
        package_prefix: Some(prefix.join(".")),
        generated: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_skips_comments_and_file_annotations() {
        assert_eq!(
            kotlin_package("// c\n/* x\n y */\n@file:JvmName(\"A\")\n\npackage a.b\n\nclass A"),
            Some("a.b".into())
        );
        assert_eq!(
            kotlin_package("/** doc */ package a.b.c;\nclass A {}"),
            Some("a.b.c".into())
        );
        assert_eq!(
            kotlin_package("#!/usr/bin/env kotlin\npackage x\n"),
            Some("x".into())
        );
        assert_eq!(kotlin_package("import a.B\nclass C"), None);
        assert_eq!(kotlin_package(""), None);
    }

    #[test]
    fn maven_layout_root_strips_package() {
        let r = source_root(
            Path::new("/ws/m/src/main/kotlin/a/b/X.kt"),
            Some("a.b"),
            false,
        );
        assert_eq!(
            r,
            SourceRoot {
                path: "/ws/m/src/main/kotlin".into(),
                package_prefix: None,
                generated: false
            }
        );
    }

    #[test]
    fn bazel_layout_root_uses_prefix() {
        let r = source_root(
            Path::new("/ws/shapes/shapes/Shape.kt"),
            Some("fixture.shapes"),
            false,
        );
        assert_eq!(
            r,
            SourceRoot {
                path: "/ws/shapes/shapes".into(),
                package_prefix: Some("fixture.shapes".into()),
                generated: false
            }
        );
    }

    #[test]
    fn default_package_root_is_the_folder() {
        let r = source_root(Path::new("/ws/x/Main.kt"), None, true);
        assert_eq!(
            r,
            SourceRoot {
                path: "/ws/x".into(),
                package_prefix: None,
                generated: true
            }
        );
    }

    fn stripped(path: &str) -> SourceRoot {
        SourceRoot {
            path: path.into(),
            package_prefix: None,
            generated: false,
        }
    }

    #[test]
    fn package_folder_below_the_stripped_root_becomes_the_root() {
        let r = package_folder_root(
            stripped("/ws/src/main/kotlin"),
            Path::new("/ws/src/main/kotlin/com/acme/core/impl/Impl.kt"),
            Path::new("/ws/src/main/kotlin/com/acme/core"),
        );
        assert_eq!(
            r,
            SourceRoot {
                path: "/ws/src/main/kotlin/com/acme/core".into(),
                package_prefix: Some("com.acme.core".into()),
                generated: false,
            }
        );
    }

    #[test]
    fn package_folder_at_or_above_the_root_or_not_holding_the_file_keeps_it() {
        let file = Path::new("/ws/src/main/kotlin/com/acme/core/Core.kt");
        for package_dir in [
            "/ws/src/main/kotlin",
            "/ws",
            "/elsewhere/pkg",
            "/ws/src/main/kotlin/com/acme/other",
        ] {
            assert_eq!(
                package_folder_root(
                    stripped("/ws/src/main/kotlin"),
                    file,
                    Path::new(package_dir)
                ),
                stripped("/ws/src/main/kotlin"),
                "{package_dir}"
            );
        }
    }

    #[test]
    fn prefixed_and_generated_roots_are_kept() {
        let prefixed = SourceRoot {
            path: "/ws/shapes".into(),
            package_prefix: Some("fixture.shapes".into()),
            generated: false,
        };
        assert_eq!(
            package_folder_root(
                prefixed.clone(),
                Path::new("/ws/shapes/sub/A.kt"),
                Path::new("/ws/shapes/sub")
            ),
            prefixed
        );
        let generated = SourceRoot {
            path: "/ob/bin".into(),
            package_prefix: None,
            generated: true,
        };
        assert_eq!(
            package_folder_root(
                generated.clone(),
                Path::new("/ob/bin/app/A.kt"),
                Path::new("/ob/bin/app")
            ),
            generated
        );
    }
}
