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
}
