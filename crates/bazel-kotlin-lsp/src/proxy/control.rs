//! The proxy's control thread: runs syncs off the message path and reports them to the editor.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bkl_lsp::write_message;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::Value;

use super::report::{Reporter, describe};
use super::schedule::Scheduler;
use super::watch::WatchFilter;
use super::{Shared, Syncer, WatchSpec, log};
use crate::bazel::Cancel;
use crate::sync::{Progress, SyncError, SyncOutcome};

/// How long stopping waits for a cancelled sync to wind down.
const STOP_WAIT: Duration = Duration::from_secs(10);

pub enum Event {
    Initialize {
        work_done_progress: bool,
        show_document: bool,
    },
    Initialized,
    /// The client's reply to a proxy request.
    Response {
        id: String,
        ok: bool,
        result: Option<Value>,
    },
    Phase(String),
    SyncDone(Box<Result<SyncOutcome, SyncError>>),
    FilesChanged(Vec<PathBuf>),
    /// The watcher lost events; anything may have changed.
    Rescan,
    Stop,
}

pub fn spawn<W: Write + Send + 'static>(
    syncer: Arc<dyn Syncer>,
    watch: Option<WatchSpec>,
    client_out: Shared<W>,
) -> (Sender<Event>, JoinHandle<()>) {
    let (tx, rx) = channel();
    let events = tx.clone();
    let handle = std::thread::spawn(move || {
        let debounce = watch.as_ref().map_or(Duration::ZERO, |w| w.debounce);
        let watching = watch.and_then(|w| start_watching(&w.root, &events));
        Control {
            syncer,
            events,
            client_out,
            reporter: Reporter::default(),
            scheduler: Scheduler::new(debounce),
            watching,
            running: None,
        }
        .run(rx)
    });
    (tx, handle)
}

struct Watching {
    root: PathBuf,
    watcher: RecommendedWatcher,
    filter: WatchFilter,
    extra: Vec<PathBuf>,
}

fn start_watching(root: &Path, events: &Sender<Event>) -> Option<Watching> {
    let tx = events.clone();
    let config = notify::Config::default().with_follow_symlinks(false);
    let handler = move |res: notify::Result<notify::Event>| match res {
        Ok(ev) if ev.need_rescan() => {
            let _ = tx.send(Event::Rescan);
        }
        Ok(ev) if !matches!(ev.kind, EventKind::Access(_)) => {
            let _ = tx.send(Event::FilesChanged(ev.paths));
        }
        Ok(_) => {}
        Err(e) => log(&format!("watching files: {e}")),
    };
    let watched = RecommendedWatcher::new(handler, config).and_then(|mut watcher| {
        watcher.watch(root, RecursiveMode::Recursive)?;
        Ok(watcher)
    });
    match watched {
        Ok(watcher) => Some(Watching {
            root: root.to_path_buf(),
            watcher,
            filter: WatchFilter::new(root),
            extra: Vec::new(),
        }),
        Err(e) => {
            log(&format!(
                "cannot watch {}, so BUILD edits will not re-sync: {e}",
                root.display()
            ));
            None
        }
    }
}

impl Watching {
    fn is_relevant(&mut self, paths: &[PathBuf]) -> bool {
        if paths
            .iter()
            .any(|p| p.file_name().is_some_and(|n| n == ".bazelignore"))
        {
            self.filter.reload_ignores();
        }
        paths.iter().any(|p| self.filter.is_relevant(p))
    }

    /// Watches the module folders that are not under the root yet.
    fn add_modules(&mut self, dirs: &[PathBuf]) {
        let real_root = self
            .root
            .canonicalize()
            .unwrap_or_else(|_| self.root.clone());
        for dir in dirs {
            let real = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            if dir.starts_with(&self.root)
                || real.starts_with(&real_root)
                || self.extra.contains(dir)
            {
                continue;
            }
            match self.watcher.watch(dir, RecursiveMode::Recursive) {
                Ok(()) => {
                    self.filter.add_root(dir);
                    self.extra.push(dir.clone());
                }
                Err(e) => log(&format!("cannot watch {}: {e}", dir.display())),
            }
        }
    }
}

struct Control<W> {
    syncer: Arc<dyn Syncer>,
    events: Sender<Event>,
    client_out: Shared<W>,
    reporter: Reporter,
    scheduler: Scheduler,
    watching: Option<Watching>,
    running: Option<Cancel>,
}

impl<W: Write> Control<W> {
    fn run(mut self, rx: Receiver<Event>) {
        self.scheduler.request_now(Instant::now());
        loop {
            if self.scheduler.poll(Instant::now()) {
                self.start_sync();
            }
            let event = match self.scheduler.deadline() {
                Some(deadline) => {
                    match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                        Ok(event) => event,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                None => match rx.recv() {
                    Ok(event) => event,
                    Err(_) => return,
                },
            };
            let out = match event {
                Event::Initialize {
                    work_done_progress,
                    show_document,
                } => {
                    self.reporter.initialize(work_done_progress, show_document);
                    vec![]
                }
                Event::Initialized => self.reporter.initialized(),
                Event::Response { id, ok, result } => {
                    self.reporter.response(&id, ok, result.as_ref())
                }
                Event::Phase(phase) => self.reporter.phase(&phase),
                Event::SyncDone(result) => {
                    self.running = None;
                    self.scheduler.finished();
                    log_result(&result);
                    if let (Ok(outcome), Some(watching)) = (&*result, &mut self.watching) {
                        watching.add_modules(&outcome.local_module_dirs);
                    }
                    self.reporter.sync_done(&result)
                }
                Event::FilesChanged(paths) => {
                    if self
                        .watching
                        .as_mut()
                        .is_some_and(|w| w.is_relevant(&paths))
                    {
                        self.scheduler.changed(Instant::now());
                    }
                    vec![]
                }
                Event::Rescan => {
                    self.scheduler.changed(Instant::now());
                    vec![]
                }
                Event::Stop => return self.stop(&rx),
            };
            self.send(&out);
        }
    }

    fn start_sync(&mut self) {
        let cancel = Cancel::default();
        self.running = Some(cancel.clone());
        let out = self.reporter.sync_started();
        self.send(&out);
        let (syncer, events) = (self.syncer.clone(), self.events.clone());
        std::thread::spawn(move || {
            let phases = events.clone();
            let progress = Progress::new(move |p| {
                let _ = phases.send(Event::Phase(p.to_string()));
            });
            let _ = events.send(Event::SyncDone(Box::new(syncer.sync(&cancel, &progress))));
        });
    }

    fn stop(self, rx: &Receiver<Event>) {
        let Some(cancel) = self.running else {
            return;
        };
        cancel.cancel();
        let deadline = Instant::now() + STOP_WAIT;
        while let Ok(event) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            if matches!(event, Event::SyncDone(_)) {
                return;
            }
        }
    }

    fn send(&self, messages: &[Value]) {
        for msg in messages {
            let _ = write_message(&mut *self.client_out.lock().unwrap(), msg);
        }
    }
}

fn log_result(result: &Result<SyncOutcome, SyncError>) {
    match result {
        Ok(o) => {
            let s = &o.summary;
            let change = if s.changed { "updated" } else { "unchanged" };
            log(&format!(
                "sync {}: {} modules, {} libraries, workspace.json {change}",
                s.status, s.modules, s.libraries
            ));
        }
        Err(e) => log(&describe("sync failed", e).replace('\n', "\n  ")),
    }
}
