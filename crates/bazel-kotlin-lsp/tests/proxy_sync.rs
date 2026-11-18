//! The proxy's own messages around a sync, with a fake syncer and a child that records what it receives.

use std::io::{BufReader, PipeWriter};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bazel_kotlin_lsp::bazel::Cancel;
use bazel_kotlin_lsp::proxy::{BazelMode, Mode, Syncer, serve};
use bazel_kotlin_lsp::sync::{Progress, SyncError, SyncOutcome, SyncSummary};
use bkl_lsp::{read_message, write_message};
use serde_json::{Value, json};

enum Step {
    Phase(&'static str),
    Finish(Box<Result<SyncOutcome, SyncError>>),
}

/// Each sync waits for the test's steps; a cancelled sync returns at once.
struct FakeSyncer {
    steps: Mutex<Receiver<Step>>,
    runs: Mutex<usize>,
    cancelled: Mutex<bool>,
}

impl Syncer for FakeSyncer {
    fn sync(&self, cancel: &Cancel, progress: &Progress) -> Result<SyncOutcome, SyncError> {
        *self.runs.lock().unwrap() += 1;
        let steps = self.steps.lock().unwrap();
        loop {
            if cancel.is_cancelled() {
                *self.cancelled.lock().unwrap() = true;
                return Err(SyncError {
                    message: "sync cancelled".into(),
                    ..Default::default()
                });
            }
            match steps.recv_timeout(Duration::from_millis(20)) {
                Ok(Step::Phase(p)) => progress.report(p),
                Ok(Step::Finish(result)) => return *result,
                Err(_) => {}
            }
        }
    }
}

struct Harness {
    to_proxy: Option<PipeWriter>,
    from_proxy: Receiver<Value>,
    steps: Sender<Step>,
    syncer: Arc<FakeSyncer>,
    done: Receiver<std::io::Result<i32>>,
    received: PathBuf,
    model_dir: PathBuf,
    _dir: tempfile::TempDir,
}

const WAIT: Duration = Duration::from_secs(10);

impl Harness {
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let received = dir.path().join("received");
        let model_dir = dir.path().join("out/model");
        let (steps, steps_rx) = channel();
        let syncer = Arc::new(FakeSyncer {
            steps: Mutex::new(steps_rx),
            runs: Mutex::new(0),
            cancelled: Mutex::new(false),
        });
        let (client_r, to_proxy) = std::io::pipe().unwrap();
        let (proxy_r, client_w) = std::io::pipe().unwrap();
        let command: Vec<String> = [
            "sh",
            "-c",
            r#"cp "$1/workspace.json" "$0.tmp" && mv "$0.tmp" "$0.model"; cat > "$0""#,
            received.to_str().unwrap(),
            model_dir.to_str().unwrap(),
        ]
        .map(String::from)
        .into();
        let mode = Mode::Bazel(BazelMode::new(model_dir.clone(), syncer.clone()));
        let (done_tx, done) = channel();
        std::thread::spawn(move || {
            done_tx
                .send(serve(client_r, client_w, &command, mode))
                .unwrap()
        });
        let (msg_tx, from_proxy) = channel();
        std::thread::spawn(move || {
            let mut r = BufReader::new(proxy_r);
            while let Ok(Some(msg)) = read_message(&mut r) {
                if msg_tx.send(msg).is_err() {
                    return;
                }
            }
        });
        Harness {
            to_proxy: Some(to_proxy),
            from_proxy,
            steps,
            syncer,
            done,
            received,
            model_dir,
            _dir: dir,
        }
    }

    fn send(&mut self, msg: Value) {
        write_message(self.to_proxy.as_mut().unwrap(), &msg).unwrap();
    }

    fn initialize(&mut self, work_done_progress: bool) {
        self.send(
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "capabilities": {"window": {"workDoneProgress": work_done_progress}}
            }}),
        );
        self.send(json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}));
    }

    fn next(&self) -> Value {
        self.from_proxy
            .recv_timeout(WAIT)
            .expect("no message from the proxy")
    }

    fn assert_quiet(&self) {
        if let Ok(msg) = self.from_proxy.recv_timeout(Duration::from_millis(300)) {
            panic!("unexpected message {msg}");
        }
    }

    fn step(&self, step: Step) {
        self.steps.send(step).unwrap();
    }

    /// Closes the editor side; returns the proxy's exit code and what the child received.
    fn finish(mut self) -> (i32, Vec<Value>) {
        drop(self.to_proxy.take());
        let code = self.done.recv_timeout(WAIT).unwrap().unwrap();
        let bytes = std::fs::read(&self.received).unwrap();
        let mut r = std::io::Cursor::new(bytes);
        let mut received = Vec::new();
        while let Some(msg) = read_message(&mut r).unwrap() {
            received.push(msg);
        }
        (code, received)
    }
}

fn outcome(status: &str, partial_reason: Option<SyncError>) -> SyncOutcome {
    SyncOutcome {
        out_dir: "/c".into(),
        model_dir: "/c/model".into(),
        summary: SyncSummary {
            status: status.into(),
            modules: 3,
            libraries: 5,
            partial_reason,
            log: "/c/sync.log".into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn progress_kind(msg: &Value, token: &Value) -> String {
    assert_eq!(msg["method"], "$/progress", "{msg}");
    assert_eq!(&msg["params"]["token"], token, "{msg}");
    msg["params"]["value"]["kind"].as_str().unwrap().to_string()
}

#[test]
fn progress_brackets_sync() {
    let mut h = Harness::start();
    h.initialize(true);
    let create = h.next();
    assert_eq!(
        create["method"], "window/workDoneProgress/create",
        "{create}"
    );
    let id = create["id"].clone();
    assert!(id.as_str().unwrap().starts_with("bkl-"), "{id}");
    let token = create["params"]["token"].clone();
    h.assert_quiet();

    h.send(json!({"jsonrpc": "2.0", "id": id, "result": null}));
    let begin = h.next();
    assert_eq!(progress_kind(&begin, &token), "begin");
    assert_eq!(begin["params"]["value"]["title"], "Bazel sync");

    h.step(Step::Phase("running the aspect"));
    let report = h.next();
    assert_eq!(progress_kind(&report, &token), "report");
    assert_eq!(report["params"]["value"]["message"], "running the aspect");

    h.step(Step::Finish(Box::new(Ok(outcome("ok", None)))));
    let end = h.next();
    assert_eq!(progress_kind(&end, &token), "end");
    assert!(
        end["params"]["value"]["message"]
            .as_str()
            .unwrap()
            .contains("3 modules"),
        "{end}"
    );
    h.assert_quiet();
    assert_eq!(h.finish().0, 0);
}

#[test]
fn client_reply_to_proxy_request_not_forwarded() {
    let mut h = Harness::start();
    h.initialize(true);
    let create = h.next();
    h.send(json!({"jsonrpc": "2.0", "id": create["id"], "result": null}));
    h.next();
    h.send(json!({"jsonrpc": "2.0", "id": 7, "result": {"applied": true}}));
    h.step(Step::Finish(Box::new(Ok(outcome("ok", None)))));
    h.next();
    let (_, received) = h.finish();
    let methods: Vec<&Value> = received.iter().map(|m| &m["method"]).collect();
    assert_eq!(
        methods,
        [&json!("initialize"), &json!("initialized"), &Value::Null]
    );
    assert_eq!(received[2]["id"], 7, "the child's own reply goes through");
}

#[test]
fn failure_shows_message_with_log_path() {
    let mut h = Harness::start();
    let tail: Vec<String> = [
        "Loading: 3 packages loaded",
        "ERROR: /ws/broken/BUILD.bazel:3:1: no such target '//nope'",
        "INFO: Elapsed time: 1.0s",
        "INFO: 0 processes.",
        "ERROR: Build did NOT complete successfully",
    ]
    .map(String::from)
    .into();
    h.step(Step::Finish(Box::new(Err(SyncError {
        message: "Bazel build failed (exit 2)".into(),
        command: Some("bazelisk build //...".into()),
        stderr_tail: tail,
        log: Some("/c/sync.log".into()),
    }))));
    h.assert_quiet();

    h.send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "capabilities": {"window": {"workDoneProgress": false, "showDocument": {"support": true}}}
    }}));
    h.send(json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}));
    let shown = h.next();
    assert_eq!(shown["method"], "window/showMessageRequest", "{shown}");
    assert_eq!(shown["params"]["type"], 1);
    assert_eq!(shown["params"]["actions"], json!([{"title": "Open log"}]));
    let text = shown["params"]["message"].as_str().unwrap();
    for expected in [
        "Bazel build failed (exit 2)",
        "ERROR: /ws/broken/BUILD.bazel:3:1: no such target '//nope'",
        "bazelisk build //...",
        "/c/sync.log",
    ] {
        assert!(text.contains(expected), "{expected:?} missing from {text}");
    }
    assert!(!text.contains("INFO: Elapsed"), "{text}");
    h.assert_quiet();

    h.send(json!({"jsonrpc": "2.0", "id": shown["id"], "result": {"title": "Open log"}}));
    let open = h.next();
    assert_eq!(open["method"], "window/showDocument", "{open}");
    assert_eq!(open["params"]["uri"], "file:///c/sync.log");
    h.send(json!({"jsonrpc": "2.0", "id": open["id"], "result": {"success": true}}));
    h.assert_quiet();
    let (_, received) = h.finish();
    assert!(
        received.iter().all(|m| m.get("result").is_none()),
        "replies to the proxy's prompt and showDocument reach the child: {received:?}"
    );
}

#[test]
fn partial_sync_shows_a_warning_with_its_reason() {
    let mut h = Harness::start();
    h.initialize(false);
    h.step(Step::Finish(Box::new(Ok(outcome(
        "partial",
        Some(SyncError {
            message: "some targets failed to build (exit 1)".into(),
            command: Some("bazelisk build --keep_going".into()),
            stderr_tail: vec!["ERROR: /ws/b/BUILD.bazel:1:1: compile failed".into()],
            log: Some("/c/sync.log".into()),
        }),
    )))));
    let shown = h.next();
    assert_eq!(shown["method"], "window/showMessageRequest", "{shown}");
    assert_eq!(shown["params"]["type"], 2);
    assert_eq!(shown["params"]["actions"], json!([{"title": "OK"}]));
    let text = shown["params"]["message"].as_str().unwrap();
    for expected in [
        "some targets failed to build",
        "compile failed",
        "bazelisk build --keep_going",
        "/c/sync.log",
    ] {
        assert!(text.contains(expected), "{expected:?} missing from {text}");
    }
    h.finish();
}

#[test]
fn placeholder_model_exists_before_the_child_starts() {
    let h = Harness::start();
    let seen = h.received.with_extension("model");
    let deadline = std::time::Instant::now() + WAIT;
    while !seen.exists() {
        assert!(std::time::Instant::now() < deadline, "child never started");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        std::fs::read_to_string(&seen).unwrap(),
        bkl_core::model::PLACEHOLDER
    );
    assert_eq!(names(&h.model_dir), ["workspace.json"]);
    h.finish();
}

#[test]
fn an_existing_model_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let model = dir.path().join("workspace.json");
    std::fs::write(&model, "{\"modules\":[1]}").unwrap();
    assert!(!bazel_kotlin_lsp::proxy::write_placeholder(dir.path()).unwrap());
    assert_eq!(
        std::fs::read_to_string(&model).unwrap(),
        "{\"modules\":[1]}"
    );
    let fresh = dir.path().join("fresh");
    assert!(bazel_kotlin_lsp::proxy::write_placeholder(&fresh).unwrap());
    assert_eq!(names(&fresh), ["workspace.json"]);
}

fn names(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn closing_the_editor_cancels_a_running_sync() {
    let h = Harness::start();
    let syncer = h.syncer.clone();
    let deadline = std::time::Instant::now() + WAIT;
    while *syncer.runs.lock().unwrap() == 0 {
        assert!(std::time::Instant::now() < deadline, "sync never started");
        std::thread::sleep(Duration::from_millis(10));
    }
    let (code, _) = h.finish();
    assert_eq!(code, 0);
    assert!(
        *syncer.cancelled.lock().unwrap(),
        "the sync was not cancelled"
    );
}

fn modified(path: &Path) -> std::time::SystemTime {
    std::fs::metadata(path).unwrap().modified().unwrap()
}

/// Waits for the placeholder model, then dates it far in the past; returns its path and that date.
fn aged_model(h: &Harness) -> (PathBuf, std::time::SystemTime) {
    let file = h.model_dir.join("workspace.json");
    let deadline = std::time::Instant::now() + WAIT;
    while !file.exists() {
        assert!(std::time::Instant::now() < deadline, "no placeholder model");
        std::thread::sleep(Duration::from_millis(20));
    }
    let old = std::time::UNIX_EPOCH + Duration::from_secs(1_000_000);
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(old)
        .unwrap();
    (file, old)
}

fn first_sync(h: &Harness, changed: bool) {
    let mut done = outcome("ok", None);
    done.model_dir = h.model_dir.clone();
    done.summary.changed = changed;
    h.step(Step::Finish(Box::new(Ok(done))));
}

#[test]
fn an_unchanged_first_sync_makes_kotlin_lsp_reimport() {
    let mut h = Harness::start();
    let (file, old) = aged_model(&h);
    h.initialize(false);
    first_sync(&h, false);
    let deadline = std::time::Instant::now() + WAIT;
    while modified(&file) == old {
        assert!(
            std::time::Instant::now() < deadline,
            "the model was not touched after an unchanged first sync"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    h.finish();
}

#[test]
fn a_rewritten_model_is_not_touched_again() {
    let mut h = Harness::start();
    let (file, old) = aged_model(&h);
    h.initialize(false);
    first_sync(&h, true);
    h.assert_quiet();
    assert_eq!(modified(&file), old);
    h.finish();
}
