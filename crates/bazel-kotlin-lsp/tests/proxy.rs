use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use bazel_kotlin_lsp::proxy::{BazelMode, Mode, serve};
use bazel_kotlin_lsp::sync::{Located, SyncOptions, locate};
use serde_json::{Value, json};

fn framed(body: &str) -> Vec<u8> {
    format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
}

/// `bazel-kotlin-lsp proxy --workspace <dir> --cache-root <dir>/cache -- sh -c <script> <args...>`.
fn spawn_proxy(dir: &Path, script: &str, args: &[&str]) -> Child {
    Command::new(env!("CARGO_BIN_EXE_bazel-kotlin-lsp"))
        .arg("proxy")
        .arg("--workspace")
        .arg(dir)
        .arg("--cache-root")
        .arg(dir.join("cache"))
        .arg("--bazel")
        .arg(dir.join("no-such-bazel"))
        .args(["--", "sh", "-c", script])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn wait_with_timeout(child: &mut Child, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("proxy still running after {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn messages(bytes: &[u8]) -> Vec<Value> {
    let mut r = std::io::Cursor::new(bytes);
    let mut out = Vec::new();
    while let Some(msg) = bkl_lsp::read_message(&mut r).unwrap() {
        out.push(msg);
    }
    out
}

fn paths(dir: &Path) -> bazel_kotlin_lsp::sync::SyncPaths {
    let opts = SyncOptions {
        start_dir: dir.to_path_buf(),
        cache_root: Some(dir.join("cache")),
        ..Default::default()
    };
    let Located::Bzlmod(paths) = locate(&opts).unwrap() else {
        panic!("not Bzlmod")
    };
    paths
}

fn model_uri(dir: &Path) -> String {
    url::Url::from_file_path(paths(dir).model_dir)
        .unwrap()
        .into()
}

/// Sends `initialize` and `exit`; returns what the child received.
fn initialize_through_proxy(dir: &Path, params: Value) -> Vec<Value> {
    let rec = dir.join("received");
    let mut proxy = spawn_proxy(dir, RECORD, &[rec.to_str().unwrap()]);
    let mut stdin = proxy.stdin.take().unwrap();
    for msg in [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": params}),
        json!({"jsonrpc": "2.0", "method": "exit"}),
    ] {
        stdin.write_all(&framed(&msg.to_string())).unwrap();
    }
    drop(stdin);
    assert!(wait_with_timeout(&mut proxy, Duration::from_secs(20)).success());
    messages(&std::fs::read(&rec).unwrap())
}

#[test]
fn kotlin_lsp_index_is_kept_in_the_workspace_cache() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    std::fs::write(dir.join("MODULE.bazel"), "").unwrap();
    let received = initialize_through_proxy(dir, json!({"rootUri": "file:///r"}));
    let index_dir = paths(dir).layout.kotlin_lsp_index_dir();
    assert_eq!(
        received[0]["params"]["initializationOptions"]["indexDir"],
        json!(index_dir.to_str().unwrap())
    );
}

#[test]
fn two_proxies_on_one_workspace_get_their_own_index() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    std::fs::write(dir.join("MODULE.bazel"), "").unwrap();
    let first_rec = dir.join("first");
    let mut first = spawn_proxy(dir, RECORD, &[first_rec.to_str().unwrap()]);
    let mut stdin = first.stdin.take().unwrap();
    let initialize = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
    stdin.write_all(&framed(&initialize.to_string())).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let first_index = loop {
        let received = std::fs::read(&first_rec)
            .map(|b| messages(&b))
            .unwrap_or_default();
        if let Some(msg) = received.first() {
            break msg["params"]["initializationOptions"]["indexDir"].clone();
        }
        assert!(
            Instant::now() < deadline,
            "the first proxy never forwarded initialize"
        );
        std::thread::sleep(Duration::from_millis(50));
    };

    let received = initialize_through_proxy(dir, json!({}));
    let second_index = &received[0]["params"]["initializationOptions"]["indexDir"];
    assert_eq!(
        first_index,
        json!(paths(dir).layout.kotlin_lsp_index_dir().to_str().unwrap())
    );
    assert!(second_index.is_string(), "{received:?}");
    assert_ne!(&first_index, second_index);

    drop(stdin);
    assert!(wait_with_timeout(&mut first, Duration::from_secs(20)).success());
}

#[test]
fn proxy_start_removes_stale_caches() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    let stale = dir.join("cache/bazel-kotlin-lsp/stale");
    std::fs::create_dir_all(stale.join("model")).unwrap();
    bkl_core::cache::write_owner(&stale, Path::new("/nonexistent/worktree"), 0).unwrap();
    initialize_through_proxy(dir, json!({}));
    assert!(!stale.exists(), "the stale cache is still there");
}

const RECORD: &str = r#"cat > "$0""#;

#[test]
fn initialize_options_are_merged() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    std::fs::write(dir.join("MODULE.bazel"), "").unwrap();
    let rec = dir.join("received");
    let mut proxy = spawn_proxy(dir, RECORD, &[rec.to_str().unwrap()]);
    let initialize = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "rootUri": "file:///r",
        "initializationOptions": {"indexDir": "/i", "projects": [{"type": "json", "path": "file:///other"}]}
    }});
    let exit = json!({"jsonrpc": "2.0", "method": "exit"});
    let mut stdin = proxy.stdin.take().unwrap();
    for msg in [&initialize, &exit] {
        stdin.write_all(&framed(&msg.to_string())).unwrap();
    }
    drop(stdin);
    assert!(wait_with_timeout(&mut proxy, Duration::from_secs(20)).success());

    let received = messages(&std::fs::read(&rec).unwrap());
    assert_eq!(received.len(), 2, "{received:#?}");
    assert_eq!(received[0]["params"]["rootUri"], "file:///r");
    assert_eq!(
        received[0]["params"]["initializationOptions"],
        json!({"indexDir": "/i", "projects": [
            {"type": "json", "path": "file:///other"},
            {"type": "json", "path": model_uri(dir)}
        ]})
    );
    assert_eq!(received[1], exit);
}

#[test]
fn passthrough_outside_bazel() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    let rec = dir.join("received");
    let reply = "Content-Length: 9\r\nX-Odd: 1\r\n\r\n{\"id\": 1}";
    let mut proxy = spawn_proxy(
        dir,
        r#"printf '%s' "$1"; cat > "$0""#,
        &[rec.to_str().unwrap(), reply],
    );
    let mut sent = b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\n".to_vec();
    sent.extend(framed(
        r#"{ "id" : 1, "method": "initialize", "params": {"initializationOptions": null} }"#,
    ));
    sent.extend(b"not even LSP");
    let mut stdin = proxy.stdin.take().unwrap();
    stdin.write_all(&sent).unwrap();
    drop(stdin);
    assert!(wait_with_timeout(&mut proxy, Duration::from_secs(20)).success());

    assert_eq!(std::fs::read(&rec).unwrap(), sent);
    let mut out = Vec::new();
    proxy.stdout.take().unwrap().read_to_end(&mut out).unwrap();
    assert_eq!(out, reply.as_bytes());
}

#[test]
fn child_exit_ends_proxy_with_its_code() {
    let t = tempfile::tempdir().unwrap();
    let bazel_repo = t.path().join("bzlmod");
    let plain = t.path().join("plain");
    std::fs::create_dir_all(&bazel_repo).unwrap();
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(bazel_repo.join("MODULE.bazel"), "").unwrap();
    for dir in [&bazel_repo, &plain] {
        let mut proxy = spawn_proxy(dir, "sleep 0.2; exit 7", &[]);
        let _stdin_kept_open = proxy.stdin.take();
        let status = wait_with_timeout(&mut proxy, Duration::from_secs(20));
        assert_eq!(status.code(), Some(7), "in {}", dir.display());
    }
}

fn alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[test]
fn closing_stdin_stops_a_child_that_ignores_it() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    std::fs::write(dir.join("MODULE.bazel"), "").unwrap();
    let pid_file: PathBuf = dir.join("pid");
    let mut proxy = spawn_proxy(
        dir,
        r#"echo $$ > "$0"; exec sleep 60"#,
        &[pid_file.to_str().unwrap()],
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !pid_file.is_file() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let pid = std::fs::read_to_string(&pid_file).unwrap();
    let pid = pid.trim();
    assert!(alive(pid));
    drop(proxy.stdin.take());
    wait_with_timeout(&mut proxy, Duration::from_secs(20));
    assert!(!alive(pid), "child {pid} outlived the proxy");
}

/// An editor that takes its time reading.
#[derive(Clone, Default)]
struct SlowEditor(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl Write for SlowEditor {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        std::thread::sleep(Duration::from_millis(100));
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn output_written_just_before_exit_reaches_the_editor() {
    let reply = framed(r#"{"jsonrpc":"2.0","id":1,"result":null}"#);
    let t = tempfile::tempdir().unwrap();
    let modes = || {
        let syncer = std::sync::Arc::new(bazel_kotlin_lsp::proxy::BazelSyncer(SyncOptions {
            start_dir: t.path().into(),
            bazel: Some(t.path().join("no-such-bazel")),
            cache_root: Some(t.path().join("cache")),
            ..Default::default()
        }));
        [
            Mode::PassThrough,
            Mode::Bazel(BazelMode::new(t.path().join("model"), syncer)),
        ]
    };
    for mode in modes() {
        let editor = SlowEditor::default();
        let (client_r, _to_proxy) = std::io::pipe().unwrap();
        let command: Vec<String> = ["sh", "-c", r#"printf '%s' "$0""#]
            .into_iter()
            .map(String::from)
            .chain([String::from_utf8(reply.clone()).unwrap()])
            .collect();
        let code = serve(client_r, editor.clone(), &command, mode).unwrap();
        assert_eq!(code, 0);
        assert_eq!(*editor.0.lock().unwrap(), reply);
    }
}

/// Runs the proxy as Zed's extension does, with `home` as HOME, through `initialized`; returns what it sent.
fn as_zed_extension(dir: &Path, home: &Path, zed_server: Option<&str>) -> Vec<Value> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bazel-kotlin-lsp"));
    command
        .arg("proxy")
        .arg("--workspace")
        .arg(dir)
        .arg("--cache-root")
        .arg(dir.join("cache"))
        .arg("--bazel")
        .arg(dir.join("no-such-bazel"));
    if let Some(name) = zed_server {
        command.args(["--zed-server", name]);
    }
    let mut proxy = command
        .args(["--", "sh", "-c", RECORD])
        .arg(dir.join("received"))
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("FLATPAK_XDG_CONFIG_HOME")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = proxy.stdin.take().unwrap();
    for msg in [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "capabilities": {"window": {"showDocument": {"support": true}}}
        }}),
        json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
    ] {
        stdin.write_all(&framed(&msg.to_string())).unwrap();
    }
    drop(stdin);
    let mut out = Vec::new();
    proxy.stdout.take().unwrap().read_to_end(&mut out).unwrap();
    assert!(wait_with_timeout(&mut proxy, Duration::from_secs(20)).success());
    messages(&out)
}

fn zed_notices(sent: &[Value]) -> Vec<&Value> {
    sent.iter()
        .filter(|m| {
            m["params"]["message"]
                .as_str()
                .is_some_and(|t| t.contains("Kotlin extension"))
        })
        .collect()
}

fn zed_home(dir: &Path, settings: &str) -> PathBuf {
    let home = dir.join("home");
    std::fs::create_dir_all(home.join(".config/zed")).unwrap();
    std::fs::write(home.join(".config/zed/settings.json"), settings).unwrap();
    home
}

#[test]
fn zed_running_kotlin_lsp_too_gets_a_notice() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    std::fs::write(dir.join("MODULE.bazel"), "").unwrap();
    let home = zed_home(dir, "// defaults\n{ \"theme\": \"One Dark\", }");
    let sent = as_zed_extension(dir, &home, Some("bazel-kotlin-lsp"));
    let notices = zed_notices(&sent);
    assert_eq!(notices.len(), 1, "{sent:#?}");
    assert_eq!(notices[0]["method"], "window/showMessageRequest");
    assert_eq!(
        notices[0]["params"]["actions"],
        json!([{"title": "Open settings"}, {"title": "Don't show again"}])
    );
}

#[test]
fn no_zed_notice_when_settings_are_right_muted_or_not_zed() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    std::fs::write(dir.join("MODULE.bazel"), "").unwrap();
    let defaults = zed_home(dir, "{}");
    assert!(zed_notices(&as_zed_extension(dir, &defaults, None)).is_empty());

    std::fs::create_dir_all(dir.join(".zed")).unwrap();
    std::fs::write(
        dir.join(".zed/settings.json"),
        r#"{"languages": {"Kotlin": {"language_servers": ["bazel-kotlin-lsp", "!kotlin-lsp", "!kotlin-language-server", "..."]}}}"#,
    )
    .unwrap();
    let sent = as_zed_extension(dir, &defaults, Some("bazel-kotlin-lsp"));
    assert!(zed_notices(&sent).is_empty(), "{sent:#?}");

    std::fs::remove_dir_all(dir.join(".zed")).unwrap();
    std::fs::write(
        dir.join("cache/bazel-kotlin-lsp/zed-settings-notice-muted"),
        "",
    )
    .unwrap();
    let sent = as_zed_extension(dir, &defaults, Some("bazel-kotlin-lsp"));
    assert!(zed_notices(&sent).is_empty(), "{sent:#?}");
}
