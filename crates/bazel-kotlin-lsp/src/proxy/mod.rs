//! `proxy`: runs kotlin-lsp behind a pipe that points it at the Bazel model.
//!
//! stdout is the editor's LSP channel: nothing but LSP messages may ever be written to it.

pub mod init;

use std::io::{self, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bkl_lsp::{read_body, write_body};
use serde::Deserialize;
use serde_json::Value;

use crate::sync::{Located, SyncOptions, locate};

/// How long the child may run on after the editor closed its input, before it is terminated.
const CHILD_GRACE: Duration = Duration::from_secs(3);
const CHILD_KILL_AFTER: Duration = Duration::from_secs(2);

pub struct ProxyOptions {
    pub sync: SyncOptions,
    pub command: Vec<String>,
}

pub enum Mode {
    /// Not a Bzlmod workspace: bytes go through unchanged.
    PassThrough,
    Bazel(BazelMode),
}

pub struct BazelMode {
    /// File URI of the folder holding `workspace.json`.
    pub model_uri: String,
}

type Shared<W> = Arc<Mutex<W>>;

pub fn log(message: &str) {
    eprintln!("bazel-kotlin-lsp: {message}");
}

/// Runs the proxy on stdin and stdout; returns the exit code.
pub fn run(opts: ProxyOptions) -> i32 {
    let mode = match locate(&opts.sync) {
        Ok(Located::Bzlmod(paths)) => Mode::Bazel(BazelMode {
            model_uri: file_uri(&paths.model_dir),
        }),
        Ok(Located::NotBzlmod(why)) => {
            log(&format!("{why}; passing messages through unchanged"));
            Mode::PassThrough
        }
        Err(e) => {
            log(&format!("{e}; passing messages through unchanged"));
            Mode::PassThrough
        }
    };
    match serve(io::stdin(), io::stdout(), &opts.command, mode) {
        Ok(code) => code,
        Err(e) => {
            log(&format!("{e}"));
            1
        }
    }
}

/// Spawns `command` and pipes the editor's messages to it and back until it exits; returns its exit code.
pub fn serve<R, W>(client_in: R, client_out: W, command: &[String], mode: Mode) -> io::Result<i32>
where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
{
    let (program, args) = command
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no kotlin-lsp command"))?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| io::Error::new(e.kind(), format!("starting {program}: {e}")))?;
    let child_in = child.stdin.take().expect("piped");
    let child_out = child.stdout.take().expect("piped");
    let (closed_tx, closed_rx) = mpsc::channel();
    let client_out: Shared<W> = Arc::new(Mutex::new(client_out));

    match mode {
        Mode::PassThrough => {
            std::thread::spawn(move || {
                copy_flushing(client_in, child_in);
                let _ = closed_tx.send(());
            });
            std::thread::spawn(move || copy_flushing(child_out, Locked(client_out)));
        }
        Mode::Bazel(bazel) => {
            std::thread::spawn(move || {
                pump_client(client_in, child_in, &bazel);
                let _ = closed_tx.send(());
            });
            std::thread::spawn(move || pump_child(child_out, &client_out));
        }
    }
    Ok(exit_code(wait_child(&mut child, &closed_rx)))
}

/// Copies bytes as they arrive, flushing each chunk, until either side closes.
fn copy_flushing(mut from: impl Read, mut to: impl Write) {
    let mut buf = [0u8; 64 * 1024];
    loop {
        match from.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                if to.write_all(&buf[..n]).and_then(|_| to.flush()).is_err() {
                    return;
                }
            }
        }
    }
}

struct Locked<W>(Shared<W>);

impl<W: Write> Write for Locked<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.lock().unwrap().flush()
    }
}

#[derive(Deserialize)]
struct Envelope {
    method: Option<String>,
}

fn pump_client(client_in: impl Read, mut child_in: ChildStdin, bazel: &BazelMode) {
    let mut reader = BufReader::new(client_in);
    loop {
        let body = match read_body(&mut reader) {
            Ok(Some(body)) => body,
            Ok(None) => return,
            Err(e) => {
                log(&format!("reading the editor's message: {e}"));
                return;
            }
        };
        let body = from_client(body, bazel);
        if write_body(&mut child_in, &body).is_err() {
            return;
        }
    }
}

fn from_client(body: Vec<u8>, bazel: &BazelMode) -> Vec<u8> {
    let Ok(envelope) = serde_json::from_slice::<Envelope>(&body) else {
        return body;
    };
    if envelope.method.as_deref() != Some("initialize") {
        return body;
    }
    let Ok(mut msg) = serde_json::from_slice::<Value>(&body) else {
        return body;
    };
    let Some(params) = msg.get_mut("params") else {
        return body;
    };
    if !init::inject_project(params, &bazel.model_uri) {
        log("initializationOptions is not an object; kotlin-lsp will not see the Bazel model");
        return body;
    }
    serde_json::to_vec(&msg).unwrap_or(body)
}

fn pump_child<W: Write>(child_out: impl Read, client_out: &Shared<W>) {
    let mut reader = BufReader::new(child_out);
    while let Ok(Some(body)) = read_body(&mut reader) {
        if write_body(&mut *client_out.lock().unwrap(), &body).is_err() {
            return;
        }
    }
}

/// Waits for the child; once the editor has closed its input, terminates it if it does not exit by itself.
fn wait_child(child: &mut Child, client_closed: &mpsc::Receiver<()>) -> ExitStatus {
    let mut closed_at: Option<Instant> = None;
    let mut terminated = false;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) => {}
            Err(_) => return child.wait().expect("waiting for kotlin-lsp"),
        }
        if closed_at.is_none() && client_closed.try_recv().is_ok() {
            closed_at = Some(Instant::now());
        }
        if let Some(at) = closed_at {
            let waited = at.elapsed();
            if waited > CHILD_GRACE + CHILD_KILL_AFTER {
                let _ = child.kill();
            } else if waited > CHILD_GRACE && !terminated {
                terminated = true;
                // SAFETY: plain kill(2) on our own child's pid, which `try_wait` has not reaped yet.
                unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn exit_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|s| 128 + s))
        .unwrap_or(1)
}

pub fn file_uri(path: &std::path::Path) -> String {
    url::Url::from_file_path(path)
        .map(String::from)
        .unwrap_or_else(|_| format!("file://{}", path.display()))
}
