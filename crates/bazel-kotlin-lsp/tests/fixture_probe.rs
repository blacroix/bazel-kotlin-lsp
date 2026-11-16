mod support;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bazel_kotlin_lsp::sync::run_sync;
use bkl_probe::cases::position_of;
use bkl_probe::client::Client;
use bkl_probe::{CaseResult, ProbeArgs, definition_case, file_uri};
use serde_json::json;
use support::Synced;

fn kotlin_lsp() -> PathBuf {
    PathBuf::from(
        std::env::var_os("KOTLIN_LSP").expect("set KOTLIN_LSP to kotlin-lsp's bin/intellij-server"),
    )
}

fn probe(root: &Path, model_dir: &Path, scratch: &Path) -> Vec<CaseResult> {
    bkl_probe::run(&probe_args(root, model_dir, scratch)).unwrap()
}

fn probe_args(root: &Path, model_dir: &Path, scratch: &Path) -> ProbeArgs {
    ProbeArgs {
        root: root.to_path_buf(),
        cases: support::repo_root().join("fixtures/basic/probe-cases.json"),
        model_dir: model_dir.to_path_buf(),
        index_dir: Some(scratch.join("index")),
        timeout: Duration::from_secs(180),
        log: Some(server_log(scratch)),
        server: vec![
            kotlin_lsp().to_string_lossy().into_owned(),
            "--stdio".into(),
            format!("--system-path={}", scratch.join("system").display()),
        ],
    }
}

/// Under `BKL_PROBE_LOG_DIR` when set (CI uploads it on failure), else in the test's scratch folder.
fn server_log(scratch: &Path) -> PathBuf {
    match std::env::var_os("BKL_PROBE_LOG_DIR") {
        Some(dir) => {
            std::fs::create_dir_all(&dir).unwrap();
            let name = scratch.file_name().unwrap().to_string_lossy();
            PathBuf::from(dir).join(format!("kotlin-lsp-{name}.log"))
        }
        None => scratch.join("server.log"),
    }
}

fn assert_all_pass(results: &[CaseResult]) {
    let failed: Vec<&CaseResult> = results.iter().filter(|r| !r.pass).collect();
    assert!(
        failed.is_empty(),
        "{}/{} passed; failures: {failed:#?}",
        results.len() - failed.len(),
        results.len()
    );
    assert_eq!(results.len(), 13);
}

#[test]
#[ignore = "needs Bazel, network and KOTLIN_LSP"]
fn probe_passes_on_fixture() {
    let s = Synced::new();
    let outcome = run_sync(&s.options()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    assert_all_pass(&probe(s.fixture.path(), &outcome.model_dir, scratch.path()));
}

#[test]
#[ignore = "needs Bazel, network and KOTLIN_LSP"]
fn probe_passes_through_symlinked_root() {
    let s = Synced::new();
    let links = tempfile::tempdir().unwrap();
    let link = links.path().join("link");
    std::os::unix::fs::symlink(s.fixture.path(), &link).unwrap();
    let mut options = s.options();
    options.start_dir = link.clone();
    let outcome = run_sync(&options).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    assert_all_pass(&probe(&link, &outcome.model_dir, scratch.path()));
}

#[test]
#[ignore = "needs Bazel, network and KOTLIN_LSP"]
fn probe_reports_type_errors() {
    let s = Synced::new();
    let broken = "app/src/main/kotlin/fixture/app/Broken.kt";
    std::fs::write(
        s.fixture.path().join(broken),
        "package fixture.app\n\nval broken: Int = \"not an int\"\n",
    )
    .unwrap();
    let outcome = run_sync(&s.options()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let cases = scratch.path().join("cases.json");
    std::fs::write(
        &cases,
        format!(r#"[{{"name":"type error","file":"{broken}","expect_no_errors":true}}]"#),
    )
    .unwrap();
    let mut args = probe_args(s.fixture.path(), &outcome.model_dir, scratch.path());
    args.cases = cases;
    args.timeout = Duration::from_secs(90);
    let results = bkl_probe::run(&args).unwrap();
    assert!(!results[0].pass, "{results:?}");
    assert!(
        results[0].targets.iter().any(|m| m.contains("mismatch")),
        "{results:?}"
    );
}

const AREA: &str = "modules/greeting/src/main/kotlin/fixture/greeting/Area.kt";

/// Asks once (no deadline) or until `deadline`; returns whether a target contains `expect`, and the targets.
fn definition(
    client: &Client,
    root: &Path,
    file: &str,
    line_contains: &str,
    symbol: &str,
    expect: &str,
    deadline: Instant,
) -> (bool, Vec<String>) {
    let path = root.join(file);
    let text = std::fs::read_to_string(&path).unwrap();
    let (line, character) = position_of(&text, line_contains, symbol).unwrap();
    definition_case(client, &file_uri(&path), line, character, expect, deadline)
}

fn open(client: &Client, root: &Path, file: &str) {
    let path = root.join(file);
    let text = std::fs::read_to_string(&path).unwrap();
    client
        .notify(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": file_uri(&path), "languageId": "kotlin", "version": 1, "text": text}}),
        )
        .unwrap();
}

/// Pins that kotlin-lsp re-imports a rewritten workspace.json by itself: if a kotlin-lsp upgrade breaks this, the
/// proxy needs to restart it after a model change.
#[test]
#[ignore = "needs Bazel, network and KOTLIN_LSP"]
fn model_change_is_picked_up_live() {
    let s = Synced::new();
    let root = s.fixture.path();
    std::fs::write(
        root.join(AREA),
        "package fixture.greeting\n\nimport fixture.shapes.Shape\n\nfun describe(shape: Shape): String = shape.name\n",
    )
    .unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let server: Vec<String> = vec![
        env!("CARGO_BIN_EXE_bazel-kotlin-lsp").into(),
        "proxy".into(),
        "--workspace".into(),
        root.to_string_lossy().into_owned(),
        "--bazel".into(),
        support::bazelisk().to_string_lossy().into_owned(),
        "--cache-root".into(),
        s.cache.path().to_string_lossy().into_owned(),
        "--".into(),
        kotlin_lsp().to_string_lossy().into_owned(),
        "--stdio".into(),
        format!("--system-path={}", scratch.path().join("system").display()),
    ];
    let client = Client::spawn(&server, Some(&server_log(scratch.path()))).unwrap();
    let init = client
        .request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": file_uri(root),
                "workspaceFolders": [{"uri": file_uri(root), "name": "fixture"}],
                "initializationOptions": {"indexDir": scratch.path().join("index")},
                "capabilities": {
                    "workspace": {"configuration": true, "workspaceFolders": true},
                    "window": {"workDoneProgress": true},
                    "textDocument": {"definition": {"linkSupport": false}, "synchronization": {}}
                }
            }),
            Duration::from_secs(300),
        )
        .unwrap();
    assert!(init.get("error").is_none(), "{init}");
    client.notify("initialized", json!({})).unwrap();
    client
        .notify("workspace/didChangeConfiguration", json!({"settings": {}}))
        .unwrap();
    let app = "app/src/main/kotlin/fixture/app/App.kt";
    open(&client, root, app);
    open(&client, root, AREA);

    let loaded = definition(
        &client,
        root,
        app,
        "import fixture.greeting.Greeter",
        "Greeter",
        "modules/greeting/src/main/kotlin/",
        Instant::now() + Duration::from_secs(400),
    );
    assert!(loaded.0, "the first sync's model never loaded: {loaded:?}");
    let shape = "modules/shapes/shapes/Shape.kt";
    let before = definition(
        &client,
        root,
        AREA,
        "fun describe",
        "Shape",
        shape,
        Instant::now(),
    );
    assert!(!before.0, "greeting already sees shapes: {before:?}");

    let greeting = root.join("modules/greeting");
    let build = std::fs::read_to_string(greeting.join("BUILD.bazel")).unwrap();
    std::fs::write(
        greeting.join("BUILD.bazel"),
        build.replace(
            "    visibility = ",
            "    deps = [\"@shapes//shapes\"],\n    visibility = ",
        ),
    )
    .unwrap();
    let mut module = std::fs::read_to_string(greeting.join("MODULE.bazel")).unwrap();
    module.push_str("bazel_dep(name = \"shapes\")\n");
    std::fs::write(greeting.join("MODULE.bazel"), module).unwrap();

    let after = definition(
        &client,
        root,
        AREA,
        "fun describe",
        "Shape",
        shape,
        Instant::now() + Duration::from_secs(60),
    );
    client.shutdown();
    assert!(
        after.0,
        "the new dependency was not picked up within 60 s: {after:?}"
    );
}
