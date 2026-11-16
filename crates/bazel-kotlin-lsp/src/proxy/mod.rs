//! `proxy`: runs kotlin-lsp behind a pipe that points it at the Bazel model.
//!
//! stdout is the editor's LSP channel: nothing but LSP messages may ever be written to it.

mod control;
pub mod init;
mod report;
mod schedule;
mod watch;

use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bkl_core::model::PLACEHOLDER;
use bkl_lsp::{read_body, write_body};
use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::Value;

use crate::bazel::Cancel;
use crate::sync::{Located, Progress, SyncError, SyncOptions, SyncOutcome, locate, run_sync};
use control::Event;

const DEBOUNCE: Duration = Duration::from_secs(2);

/// How long the child may run on after the editor closed its input, before it is terminated.
const CHILD_GRACE: Duration = Duration::from_secs(3);
const CHILD_KILL_AFTER: Duration = Duration::from_secs(2);
/// How long the child's last output may take to reach the editor after the child exited.
const DRAIN_WAIT: Duration = Duration::from_secs(5);

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
    /// The folder holding `workspace.json`.
    model_dir: PathBuf,
    model_uri: String,
    syncer: Arc<dyn Syncer>,
    watch: Option<WatchSpec>,
}

pub struct WatchSpec {
    pub root: PathBuf,
    /// Quiet time after the last relevant change before a sync starts.
    pub debounce: Duration,
}

impl BazelMode {
    pub fn new(model_dir: PathBuf, syncer: Arc<dyn Syncer>) -> Self {
        BazelMode {
            model_uri: file_uri(&model_dir),
            model_dir,
            syncer,
            watch: None,
        }
    }

    /// Re-syncs after build file changes under `root` and the local modules.
    pub fn with_watch(mut self, root: PathBuf, debounce: Duration) -> Self {
        self.watch = Some(WatchSpec { root, debounce });
        self
    }
}

/// Runs one sync; the proxy runs at most one at a time, off the editor's message path.
pub trait Syncer: Send + Sync + 'static {
    fn sync(&self, cancel: &Cancel, progress: &Progress) -> Result<SyncOutcome, SyncError>;
}

pub struct BazelSyncer(pub SyncOptions);

impl Syncer for BazelSyncer {
    fn sync(&self, cancel: &Cancel, progress: &Progress) -> Result<SyncOutcome, SyncError> {
        let mut options = self.0.clone();
        options.cancel = Some(cancel.clone());
        options.progress = Some(progress.clone());
        run_sync(&options)
    }
}

/// Writes an empty model unless one exists, so kotlin-lsp can import something before the first sync ends.
/// Returns whether it wrote.
pub fn write_placeholder(model_dir: &Path) -> io::Result<bool> {
    let file = model_dir.join("workspace.json");
    if file.exists() {
        return Ok(false);
    }
    std::fs::create_dir_all(model_dir)?;
    let tmp = model_dir.join(format!("workspace.json.tmp-{}", std::process::id()));
    std::fs::write(&tmp, PLACEHOLDER)?;
    // A hard link never replaces a model that a concurrent sync wrote meanwhile.
    let linked = std::fs::hard_link(&tmp, &file);
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

type Shared<W> = Arc<Mutex<W>>;

pub fn log(message: &str) {
    eprintln!("bazel-kotlin-lsp: {message}");
}

/// Runs the proxy on stdin and stdout; returns the exit code.
pub fn run(opts: ProxyOptions) -> i32 {
    let mode = match locate(&opts.sync) {
        Ok(Located::Bzlmod(paths)) => Mode::Bazel(
            BazelMode::new(paths.model_dir, Arc::new(BazelSyncer(opts.sync)))
                .with_watch(paths.root, DEBOUNCE),
        ),
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
    if let Mode::Bazel(bazel) = &mode
        && let Err(e) = write_placeholder(&bazel.model_dir)
    {
        log(&format!(
            "writing a placeholder model in {}: {e}",
            bazel.model_dir.display()
        ));
    }
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
    let (drained_tx, drained_rx) = mpsc::channel::<()>();
    let client_out: Shared<W> = Arc::new(Mutex::new(client_out));

    let control = match mode {
        Mode::PassThrough => {
            std::thread::spawn(move || {
                copy_flushing(client_in, child_in);
                let _ = closed_tx.send(());
            });
            std::thread::spawn(move || {
                copy_flushing(child_out, Locked(client_out));
                drop(drained_tx);
            });
            None
        }
        Mode::Bazel(mut bazel) => {
            let (events, handle) =
                control::spawn(bazel.syncer.clone(), bazel.watch.take(), client_out.clone());
            let to_control = events.clone();
            std::thread::spawn(move || {
                pump_client(client_in, child_in, &bazel.model_uri, &to_control);
                let _ = closed_tx.send(());
            });
            std::thread::spawn(move || {
                pump_child(child_out, &client_out);
                drop(drained_tx);
            });
            Some((events, handle))
        }
    };
    let status = wait_child(&mut child, &closed_rx);
    let _ = drained_rx.recv_timeout(DRAIN_WAIT);
    if let Some((events, handle)) = control {
        let _ = events.send(Event::Stop);
        let _ = handle.join();
    }
    Ok(exit_code(status))
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
    id: Option<Value>,
    error: Option<IgnoredAny>,
    result: Option<Value>,
}

fn pump_client(
    client_in: impl Read,
    mut child_in: ChildStdin,
    model_uri: &str,
    control: &Sender<Event>,
) {
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
        let Some(body) = from_client(body, model_uri, control) else {
            continue;
        };
        if write_body(&mut child_in, &body).is_err() {
            return;
        }
    }
}

/// What to forward to kotlin-lsp for one client message; `None` for replies to the proxy's own requests.
fn from_client(body: Vec<u8>, model_uri: &str, control: &Sender<Event>) -> Option<Vec<u8>> {
    let Ok(envelope) = serde_json::from_slice::<Envelope>(&body) else {
        return Some(body);
    };
    match (envelope.method.as_deref(), &envelope.id) {
        (None, Some(Value::String(id))) if id.starts_with("bkl-") => {
            let _ = control.send(Event::Response {
                id: id.clone(),
                ok: envelope.error.is_none(),
                result: envelope.result,
            });
            None
        }
        (Some("initialize"), _) => Some(initialize(body, model_uri, control)),
        (Some("initialized"), _) => {
            let _ = control.send(Event::Initialized);
            Some(body)
        }
        _ => Some(body),
    }
}

fn initialize(body: Vec<u8>, model_uri: &str, control: &Sender<Event>) -> Vec<u8> {
    let Ok(mut msg) = serde_json::from_slice::<Value>(&body) else {
        return body;
    };
    let work_done_progress = msg
        .pointer("/params/capabilities/window/workDoneProgress")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let show_document = msg
        .pointer("/params/capabilities/window/showDocument/support")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let _ = control.send(Event::Initialize {
        work_done_progress,
        show_document,
    });
    let Some(params) = msg.get_mut("params") else {
        return body;
    };
    if !init::inject_project(params, model_uri) {
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

pub fn file_uri(path: &Path) -> String {
    url::Url::from_file_path(path)
        .map(String::from)
        .unwrap_or_else(|_| format!("file://{}", path.display()))
}
