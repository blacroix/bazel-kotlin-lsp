mod support;

use std::process::Command;

use bazel_kotlin_lsp::sync::run_sync;
use support::{Synced, dep_names, module, workspace_json};

#[test]
#[ignore = "needs Bazel and network"]
fn sync_fixture_produces_expected_model() {
    let s = Synced::new();
    let outcome = run_sync(&s.options()).unwrap();
    assert_eq!(outcome.summary.status, "ok", "{:?}", outcome.summary);
    let ws = workspace_json(&outcome.model_dir);

    let mut names: Vec<&str> = ws["modules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    names.sort();
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
    let feature = module(
        &ws,
        "fixture.layout.src.main.kotlin.fixture.layout.feature.feature",
    );
    assert!(
        dep_names(feature)
            .contains(&"fixture.layout.src.main.kotlin.fixture.layout.core.core".to_string())
    );

    let app = dep_names(module(&ws, "fixture.app.app"));
    for expected in [
        "greeting.greeting",
        "shapes.shapes.shapes",
        "@@//gen:version",
    ] {
        assert!(
            app.iter().any(|d| d == expected),
            "{expected} not in {app:?}"
        );
    }
    let home = ws["sdks"][0]["homePath"].as_str().unwrap();
    assert!(
        std::path::Path::new(home).join("release").is_file(),
        "{home}"
    );
    for library in ws["libraries"].as_array().unwrap() {
        for root in library["roots"].as_array().unwrap() {
            let path = root["path"].as_str().unwrap();
            assert!(std::path::Path::new(path).exists(), "{path}");
        }
    }
    let settings = ws["kotlinSettings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["module"] == "fixture.app.app")
        .unwrap();
    let args = settings["compilerArguments"].as_str().unwrap();
    assert!(
        args.contains(r#""optIn":["kotlinx.serialization.ExperimentalSerializationApi"]"#),
        "{args}"
    );
    assert!(
        args.contains("kotlinx-serialization-compiler-plugin"),
        "{args}"
    );
}

#[test]
#[ignore = "needs Bazel and network"]
fn second_sync_is_noop() {
    let s = Synced::new();
    let first = run_sync(&s.options()).unwrap();
    assert!(first.summary.changed);
    let file = first.model_dir.join("workspace.json");
    let before = std::fs::metadata(&file).unwrap().modified().unwrap();
    let second = run_sync(&s.options()).unwrap();
    assert!(!second.summary.changed);
    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().unwrap(),
        before
    );
}

#[test]
#[ignore = "needs Bazel and network"]
fn build_file_edit_changes_model() {
    let s = Synced::new();
    run_sync(&s.options()).unwrap();
    let greeting = s.fixture.path().join("modules/greeting");
    let module_file = greeting.join("MODULE.bazel");
    let text = std::fs::read_to_string(&module_file).unwrap();
    std::fs::write(&module_file, text + "bazel_dep(name = \"shapes\")\n").unwrap();
    let build_file = greeting.join("BUILD.bazel");
    let text = std::fs::read_to_string(&build_file).unwrap();
    std::fs::write(
        &build_file,
        text.replace(
            "    visibility = [\"//visibility:public\"],\n)",
            "    visibility = [\"//visibility:public\"],\n    deps = [\"@shapes//shapes\"],\n)",
        ),
    )
    .unwrap();

    let outcome = run_sync(&s.options()).unwrap();
    assert!(outcome.summary.changed);
    let ws = workspace_json(&outcome.model_dir);
    assert!(
        dep_names(module(&ws, "greeting.greeting")).contains(&"shapes.shapes.shapes".to_string())
    );
}

#[test]
#[ignore = "needs Bazel and network"]
fn broken_target_gives_partial() {
    let s = Synced::new();
    let mut options = s.options();
    options.scope_dirs = vec![".".into()];
    options.targets = vec!["//broken:broken".into()];
    let outcome = run_sync(&options).unwrap();
    assert_eq!(outcome.summary.status, "partial");
    module(&workspace_json(&outcome.model_dir), "fixture.app.app");
}

#[test]
#[ignore = "needs Bazel and network"]
fn repo_is_left_untouched() {
    let s = Synced::new();
    run_sync(&s.options()).unwrap();
    run_sync(&s.options()).unwrap();
    let status = Command::new("git")
        .args(["status", "--porcelain", "--ignored"])
        .current_dir(s.fixture.path())
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&status.stdout), "");
}

#[test]
#[ignore = "needs Bazel and network"]
fn scope_flag_turns_module_into_library() {
    let s = Synced::new();
    let mut options = s.options();
    options.scope_dirs = vec!["app".into()];
    let outcome = run_sync(&options).unwrap();
    let ws = workspace_json(&outcome.model_dir);
    assert!(
        ws["modules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["name"] != "greeting.greeting")
    );
    let library = ws["libraries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["name"] == "@@greeting+//:greeting")
        .unwrap();
    let roots = library["roots"].as_array().unwrap();
    assert!(roots.iter().any(|r| r.get("type").is_none()));
    assert!(roots.iter().any(|r| r["type"] == "SOURCES"));
    for r in roots {
        assert!(std::path::Path::new(r["path"].as_str().unwrap()).exists());
    }
}

#[test]
#[ignore = "needs Bazel and network"]
fn scope_on_one_package_keeps_its_neighbour_out() {
    let s = Synced::new();
    let mut options = s.options();
    options.scope_dirs = vec!["layout/src/main/kotlin/fixture/layout/feature".into()];
    let outcome = run_sync(&options).unwrap();
    let ws = workspace_json(&outcome.model_dir);
    let modules = ws["modules"].as_array().unwrap();
    assert_eq!(modules.len(), 1, "{modules:?}");
    let roots: Vec<&str> = modules[0]["contentRoots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();
    assert_eq!(roots.len(), 1, "{roots:?}");
    assert!(
        roots[0].ends_with("layout/src/main/kotlin/fixture/layout/feature"),
        "{roots:?}"
    );
    assert!(
        ws["libraries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["name"] == "@@//layout/src/main/kotlin/fixture/layout/core:core")
    );
}

/// Tools that keep worktrees inside the checkout put whole Bazel workspaces under the root; `//...` would build
/// them as packages of this one, against a MODULE.bazel that may lack their repositories.
#[test]
#[ignore = "needs Bazel and network"]
fn a_workspace_nested_in_the_repository_is_left_out() {
    let s = Synced::new();
    let copy = s.fixture.path().join(".worktrees/feature/repo");
    std::fs::create_dir_all(copy.join("testing")).unwrap();
    std::fs::write(copy.join("MODULE.bazel"), "module(name = \"copy\")\n").unwrap();
    std::fs::write(
        copy.join("testing/BUILD.bazel"),
        "load(\"@rules_kotlin//kotlin:jvm.bzl\", \"kt_jvm_library\")\n\
         kt_jvm_library(name = \"lib\", srcs = [\"Lib.kt\"], deps = [\"@unknown_maven//:junit\"])\n",
    )
    .unwrap();
    std::fs::write(copy.join("testing/Lib.kt"), "package copy\n").unwrap();
    let standalone = s.fixture.path().join("standalone");
    std::fs::create_dir_all(&standalone).unwrap();
    std::fs::write(standalone.join("MODULE.bazel"), "").unwrap();

    let outcome = run_sync(&s.options()).unwrap();
    assert_eq!(outcome.summary.status, "ok", "{:?}", outcome.summary);
    let ws = workspace_json(&outcome.model_dir);
    let roots: Vec<&str> = ws["modules"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["contentRoots"].as_array().unwrap())
        .map(|r| r["path"].as_str().unwrap())
        .collect();
    assert!(!roots.is_empty());
    assert!(roots.iter().all(|r| !r.contains(".worktrees")), "{roots:?}");
}
