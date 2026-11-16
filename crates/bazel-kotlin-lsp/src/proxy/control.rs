//! The proxy's control thread: runs syncs off the message path and reports them to the editor.

use std::io::Write;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bkl_lsp::write_message;
use serde_json::Value;

use super::report::{Reporter, describe};
use super::{Shared, Syncer, log};
use crate::bazel::Cancel;
use crate::sync::{Progress, SyncError, SyncOutcome};

/// How long stopping waits for a cancelled sync to wind down.
const STOP_WAIT: Duration = Duration::from_secs(10);

pub enum Event {
    Initialize {
        work_done_progress: bool,
    },
    Initialized,
    /// The client's reply to a proxy request.
    Response {
        id: String,
        ok: bool,
    },
    Phase(String),
    SyncDone(Box<Result<SyncOutcome, SyncError>>),
    Stop,
}

pub fn spawn<W: Write + Send + 'static>(
    syncer: Arc<dyn Syncer>,
    client_out: Shared<W>,
) -> (Sender<Event>, JoinHandle<()>) {
    let (tx, rx) = channel();
    let events = tx.clone();
    let handle = std::thread::spawn(move || {
        Control {
            syncer,
            events,
            client_out,
            reporter: Reporter::default(),
            running: None,
        }
        .run(rx)
    });
    (tx, handle)
}

struct Control<W> {
    syncer: Arc<dyn Syncer>,
    events: Sender<Event>,
    client_out: Shared<W>,
    reporter: Reporter,
    running: Option<Cancel>,
}

impl<W: Write> Control<W> {
    fn run(mut self, rx: Receiver<Event>) {
        self.start_sync();
        while let Ok(event) = rx.recv() {
            let out = match event {
                Event::Initialize { work_done_progress } => {
                    self.reporter.initialize(work_done_progress);
                    vec![]
                }
                Event::Initialized => self.reporter.initialized(),
                Event::Response { id, ok } => self.reporter.response(&id, ok),
                Event::Phase(phase) => self.reporter.phase(&phase),
                Event::SyncDone(result) => {
                    self.running = None;
                    log_result(&result);
                    self.reporter.sync_done(&result)
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
