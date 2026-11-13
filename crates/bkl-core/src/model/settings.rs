use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{Fs, ModelContext, ModuleGroup, resolve_exec_path};
use crate::aspect::{KotlincInfo, TargetInfo};
use crate::workspace_json::{KotlinSettings, Sdk};

pub(crate) const SDK_NAME: &str = "bazel-jdk";

/// The major version in a JDK `release` file (`JAVA_VERSION="21.0.5"` -> 21, `"1.8.0_402"` -> 8).
pub fn java_major(release_file: &str) -> Option<u32> {
    let version = release_file
        .lines()
        .find_map(|l| l.trim().strip_prefix("JAVA_VERSION="))?
        .trim_matches('"');
    let mut parts = version.split(['.', '_', '-', '+']);
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}

pub(crate) fn sdk(
    targets: &[TargetInfo],
    ctx: &ModelContext,
    fs: &dyn Fs,
    warnings: &mut Vec<String>,
) -> Option<Sdk> {
    let from_toolchain = targets
        .iter()
        .find_map(|t| t.java_home.as_deref())
        .map(|h| resolve_exec_path(ctx, h));
    if let Some(sdk) = from_toolchain.as_deref().and_then(|home| jdk_at(home, fs)) {
        return Some(sdk);
    }
    if let Some(sdk) = ctx
        .java_home_fallback
        .as_deref()
        .and_then(|home| jdk_at(home, fs))
    {
        warnings.push(format!(
            "no JDK from the Bazel Java toolchain; using {}",
            sdk.home_path
        ));
        return Some(sdk);
    }
    warnings.push(
        "no JDK found (Bazel Java toolchain or JAVA_HOME); JDK classes will not resolve".into(),
    );
    None
}

fn jdk_at(home: &Path, fs: &dyn Fs) -> Option<Sdk> {
    [home.to_path_buf(), home.join("Contents/Home")]
        .into_iter()
        .find_map(|home: PathBuf| {
            let major = java_major(&fs.read_head(&home.join("release"))?)?;
            Some(Sdk {
                name: SDK_NAME.into(),
                sdk_type: "JavaSDK".into(),
                version: format!("JDK_{major}"),
                home_path: home.to_string_lossy().into_owned(),
                additional_data: String::new(),
            })
        })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CompilerArguments {
    #[serde(skip_serializing_if = "Option::is_none")]
    language_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    jvm_target: Option<String>,
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    opt_in: BTreeSet<String>,
    plugin_options: BTreeSet<String>,
    plugin_classpaths: BTreeSet<String>,
}

/// kotlin-lsp's `compilerArguments`: `J` followed by the K2 compiler arguments as JSON.
pub(crate) fn compiler_arguments(members: &[&TargetInfo], ctx: &ModelContext) -> String {
    let infos: Vec<&KotlincInfo> = members.iter().filter_map(|t| t.kotlinc.as_ref()).collect();
    let first = |f: fn(&KotlincInfo) -> &Option<String>| infos.iter().find_map(|i| f(i).clone());
    let args = CompilerArguments {
        language_version: first(|i| &i.language_version),
        api_version: first(|i| &i.api_version),
        jvm_target: first(|i| &i.jvm_target),
        opt_in: infos
            .iter()
            .flat_map(|i| i.opt_in.iter().cloned())
            .collect(),
        plugin_options: infos
            .iter()
            .flat_map(|i| i.plugin_options.iter().cloned())
            .collect(),
        plugin_classpaths: infos
            .iter()
            .flat_map(|i| i.plugin_classpath.iter())
            .map(|p| resolve_exec_path(ctx, p).to_string_lossy().into_owned())
            .collect(),
    };
    format!(
        "J{}",
        serde_json::to_string(&args).expect("plain data serializes")
    )
}

pub(crate) fn kotlin_settings(
    group: &ModuleGroup,
    associates: Vec<String>,
    compiler_arguments: String,
) -> KotlinSettings {
    KotlinSettings {
        name: "Kotlin".into(),
        source_roots: group
            .roots
            .keys()
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
        config_file_items: vec![],
        module: group.name.clone(),
        use_project_settings: false,
        implemented_module_names: vec![],
        depends_on_module_names: vec![],
        additional_visible_module_names: associates,
        production_output_path: None,
        test_output_path: None,
        source_set_names: vec![],
        is_test_module: group.is_test,
        external_project_id: group.name.clone(),
        is_hmpp_enabled: true,
        pure_kotlin_source_folders: vec![],
        kind: "default".into(),
        compiler_arguments,
        additional_arguments: None,
        script_templates: None,
        script_templates_classpath: None,
        output_directory_for_js_library_files: None,
        target_platform: None,
        external_system_run_tasks: vec![],
        version: 5,
        flush_needed: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::testing::{FakeFs, ctx, target};

    #[test]
    fn java_major_variants() {
        assert_eq!(
            java_major("IMPLEMENTOR=\"x\"\nJAVA_VERSION=\"21.0.5\"\n"),
            Some(21)
        );
        assert_eq!(java_major("JAVA_VERSION=\"1.8.0_402\""), Some(8));
        assert_eq!(java_major("JAVA_VERSION=\"25\""), Some(25));
        assert_eq!(java_major("nothing"), None);
    }

    #[test]
    fn sdk_from_toolchain_java_home() {
        let mut t = target("@@//app:app", "kt_jvm_library");
        t.java_home = Some("external/jdk21".into());
        let fs = FakeFs::with(&[("/ob/external/jdk21/release", "JAVA_VERSION=\"21.0.5\"")]);
        let mut warnings = Vec::new();
        let sdk = sdk(&[t], &ctx(), &fs, &mut warnings).unwrap();
        assert_eq!(sdk.version, "JDK_21");
        assert_eq!(sdk.home_path, "/ob/external/jdk21");
        assert_eq!(sdk.name, "bazel-jdk");
        assert!(warnings.is_empty());
    }

    #[test]
    fn sdk_tries_macos_bundle_layout() {
        let mut t = target("@@//app:app", "kt_jvm_library");
        t.java_home = Some("external/jdk".into());
        let fs = FakeFs::with(&[(
            "/ob/external/jdk/Contents/Home/release",
            "JAVA_VERSION=\"17.0.1\"",
        )]);
        let sdk = sdk(&[t], &ctx(), &fs, &mut Vec::new()).unwrap();
        assert_eq!(sdk.home_path, "/ob/external/jdk/Contents/Home");
    }

    #[test]
    fn sdk_falls_back_to_java_home_with_warning() {
        let mut c = ctx();
        c.java_home_fallback = Some("/opt/jdk".into());
        let fs = FakeFs::with(&[("/opt/jdk/release", "JAVA_VERSION=\"17.0.1\"")]);
        let mut warnings = Vec::new();
        let sdk = sdk(
            &[target("@@//app:app", "kt_jvm_library")],
            &c,
            &fs,
            &mut warnings,
        )
        .unwrap();
        assert_eq!(sdk.version, "JDK_17");
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn no_jdk_at_all_warns() {
        let mut warnings = Vec::new();
        assert!(sdk(&[], &ctx(), &FakeFs::default(), &mut warnings).is_none());
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn compiler_arguments_shape() {
        let mut a = target("@@//a:a", "kt_jvm_library");
        a.kotlinc = Some(KotlincInfo {
            language_version: Some("2.4".into()),
            api_version: Some("2.4".into()),
            jvm_target: Some("21".into()),
            opt_in: vec!["b.B".into(), "a.A".into()],
            plugin_classpath: vec!["external/k/plugin.jar".into()],
            plugin_options: vec!["plugin:p:k=v".into()],
        });
        let mut b = target("@@//a:b", "kt_jvm_test");
        b.kotlinc = Some(KotlincInfo {
            opt_in: vec!["a.A".into()],
            ..Default::default()
        });
        assert_eq!(
            compiler_arguments(&[&a, &b], &ctx()),
            r#"J{"languageVersion":"2.4","apiVersion":"2.4","jvmTarget":"21","optIn":["a.A","b.B"],"pluginOptions":["plugin:p:k=v"],"pluginClasspaths":["/ob/external/k/plugin.jar"]}"#
        );
        assert_eq!(
            compiler_arguments(&[&target("@@//j:j", "java_library")], &ctx()),
            r#"J{"pluginOptions":[],"pluginClasspaths":[]}"#
        );
    }
}
