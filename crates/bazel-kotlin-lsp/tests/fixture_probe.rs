mod support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use bazel_kotlin_lsp::sync::run_sync;
use bkl_probe::{CaseResult, ProbeArgs};
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
        log: Some(scratch.join("server.log")),
        server: vec![
            kotlin_lsp().to_string_lossy().into_owned(),
            "--stdio".into(),
            format!("--system-path={}", scratch.join("system").display()),
        ],
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
    assert_eq!(results.len(), 11);
}

#[test]
#[ignore = "needs Bazel, network and KOTLIN_LSP"]
fn probe_passes_on_fixture() {
    let s = Synced::new();
    let outcome = run_sync(&s.options()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    assert_all_pass(&probe(s.fixture.path(), &outcome.out_dir, scratch.path()));
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
    assert_all_pass(&probe(&link, &outcome.out_dir, scratch.path()));
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
    let mut args = probe_args(s.fixture.path(), &outcome.out_dir, scratch.path());
    args.cases = cases;
    args.timeout = Duration::from_secs(90);
    let results = bkl_probe::run(&args).unwrap();
    assert!(!results[0].pass, "{results:?}");
    assert!(
        results[0].targets.iter().any(|m| m.contains("mismatch")),
        "{results:?}"
    );
}
