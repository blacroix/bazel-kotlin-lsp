use std::path::{Path, PathBuf};

use bkl_core::aspect::read_targets;
use bkl_core::bazel_mod::LocalModule;
use bkl_core::model::{Fs, ModelContext, build_model, render};
use bkl_core::scope::Scope;

/// `/ws/**` reads the fixture sources on disk; generated files and the JDK release file are faked; every path
/// exists.
struct FixtureFs {
    fixture: PathBuf,
}

impl Fs for FixtureFs {
    fn exists(&self, _: &Path) -> bool {
        true
    }

    fn read_head(&self, p: &Path) -> Option<String> {
        let name = p.file_name()?.to_str()?;
        match name {
            "release" => Some("JAVA_VERSION=\"21.0.8\"\n".into()),
            "BuildInfo.kt" => Some("package fixture.app\n".into()),
            "Version.kt" => Some("package fixture.gen\n".into()),
            _ => std::fs::read_to_string(self.fixture.join(p.strip_prefix("/ws").ok()?)).ok(),
        }
    }
}

fn model() -> bkl_core::model::Model {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files: Vec<PathBuf> = std::fs::read_dir(manifest.join("tests/data/fixture-aspect"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    let targets = read_targets(&files).unwrap();
    let ctx = ModelContext {
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
        kotlin_stdlib: Some("@@rules_kotlin+//kotlin/compiler:kotlin-stdlib".into()),
        java_home_fallback: None,
    };
    build_model(
        &targets,
        &ctx,
        &FixtureFs {
            fixture: manifest.join("../../fixtures/basic"),
        },
    )
}

#[test]
fn golden_fixture() {
    let model = model();
    assert!(model.warnings.is_empty(), "{:?}", model.warnings);
    insta::assert_snapshot!(String::from_utf8(render(&model.workspace)).unwrap());
}

#[test]
fn render_is_stable() {
    assert_eq!(render(&model().workspace), render(&model().workspace));
}

#[test]
fn fixture_module_set() {
    let names: Vec<String> = model()
        .workspace
        .modules
        .iter()
        .map(|m| m.name.clone())
        .collect();
    assert_eq!(
        names,
        [
            "fixture.app.app",
            "fixture.app.app_test",
            "fixture.layout.src.main.kotlin.fixture.layout.core.core",
            "fixture.layout.src.main.kotlin.fixture.layout.feature.feature",
            "greeting.greeting",
            "shapes.shapes.shapes"
        ]
    );
}
