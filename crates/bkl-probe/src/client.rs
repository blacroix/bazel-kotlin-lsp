//! A minimal LSP client over a child process's stdio.

use std::collections::HashMap;
use std::io::BufReader;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use bkl_lsp::{read_message, write_message};
use serde_json::{Value, json};

#[derive(Default)]
struct State {
    responses: HashMap<i64, Value>,
    diagnostics: HashMap<String, Vec<Value>>,
    closed: bool,
}

pub struct Client {
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    state: Arc<(Mutex<State>, Condvar)>,
    next_id: AtomicI64,
}

impl Client {
    pub fn spawn(server: &[String], log: Option<&Path>) -> anyhow::Result<Self> {
        let (program, args) = server
            .split_first()
            .ok_or_else(|| anyhow!("no server command"))?;
        let stderr = match log {
            Some(path) => Stdio::from(
                std::fs::File::create(path).with_context(|| format!("{}", path.display()))?,
            ),
            None => Stdio::null(),
        };
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .with_context(|| format!("starting {program}"))?;
        let stdin = Arc::new(Mutex::new(child.stdin.take().expect("piped")));
        let state: Arc<(Mutex<State>, Condvar)> = Arc::default();
        let mut stdout = BufReader::new(child.stdout.take().expect("piped"));
        let (reader_state, reader_stdin) = (state.clone(), stdin.clone());
        std::thread::spawn(move || {
            while let Ok(Some(msg)) = read_message(&mut stdout) {
                dispatch(msg, &reader_state, &reader_stdin);
            }
            let (lock, cvar) = &*reader_state;
            lock.lock().unwrap().closed = true;
            cvar.notify_all();
        });
        Ok(Client {
            child,
            stdin,
            state,
            next_id: AtomicI64::new(1),
        })
    }

    pub fn notify(&self, method: &str, params: Value) -> anyhow::Result<()> {
        let msg = json!({"jsonrpc": "2.0", "method": method, "params": params});
        Ok(write_message(&mut *self.stdin.lock().unwrap(), &msg)?)
    }

    /// The full response message (with `result` or `error`).
    pub fn request(&self, method: &str, params: Value, timeout: Duration) -> anyhow::Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        write_message(&mut *self.stdin.lock().unwrap(), &msg)?;
        let (lock, cvar) = &*self.state;
        let deadline = Instant::now() + timeout;
        let mut state = lock.lock().unwrap();
        loop {
            if let Some(response) = state.responses.remove(&id) {
                return Ok(response);
            }
            if state.closed {
                return Err(anyhow!("server exited while waiting for {method}"));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(anyhow!("{method} timed out after {timeout:?}"));
            }
            state = cvar.wait_timeout(state, left).unwrap().0;
        }
    }

    /// The last published diagnostics for a URI, if any were published.
    pub fn published_diagnostics(&self, uri: &str) -> Option<Vec<Value>> {
        self.state.0.lock().unwrap().diagnostics.get(uri).cloned()
    }

    pub fn shutdown(mut self) {
        let _ = self.request("shutdown", Value::Null, Duration::from_secs(30));
        let _ = self.notify("exit", Value::Null);
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn dispatch(msg: Value, state: &Arc<(Mutex<State>, Condvar)>, stdin: &Arc<Mutex<ChildStdin>>) {
    let method = msg.get("method").and_then(Value::as_str);
    let id = msg.get("id").cloned();
    match (method, id) {
        (None, Some(id)) => {
            if let Some(id) = id.as_i64() {
                let (lock, cvar) = &**state;
                lock.lock().unwrap().responses.insert(id, msg);
                cvar.notify_all();
            }
        }
        (Some(method), Some(id)) => {
            let result = if method == "workspace/configuration" {
                let n = msg
                    .pointer("/params/items")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                Value::Array(vec![Value::Null; n])
            } else {
                Value::Null
            };
            let _ = write_message(
                &mut *stdin.lock().unwrap(),
                &json!({"jsonrpc": "2.0", "id": id, "result": result}),
            );
        }
        (Some("textDocument/publishDiagnostics"), None) => {
            let uri = msg
                .pointer("/params/uri")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let diagnostics = msg
                .pointer("/params/diagnostics")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            state.0.lock().unwrap().diagnostics.insert(uri, diagnostics);
        }
        _ => {}
    }
}
