//! What the proxy tells the editor about syncs: `$/progress` and `window/showMessage`.
//!
//! Nothing is sent before the client's `initialized` notification, and progress only to clients that advertise
//! `window.workDoneProgress`. A progress token is used only once the client has answered its creation request.

use serde_json::{Value, json};

use crate::sync::{SyncError, SyncOutcome};

const TITLE: &str = "Bazel sync";
const SHOWN_LINES: usize = 5;
const ERROR: u8 = 1;
const WARNING: u8 = 2;

#[derive(Default)]
pub struct Reporter {
    ready: bool,
    work_done_progress: bool,
    syncing: bool,
    phase: Option<String>,
    requests: u64,
    progress: ProgressState,
    queued: Vec<Value>,
}

#[derive(Default)]
enum ProgressState {
    #[default]
    Idle,
    Creating {
        id: String,
        token: String,
    },
    Active {
        token: String,
    },
}

impl Reporter {
    pub fn initialize(&mut self, work_done_progress: bool) {
        self.work_done_progress = work_done_progress;
    }

    pub fn initialized(&mut self) -> Vec<Value> {
        self.ready = true;
        let mut out = std::mem::take(&mut self.queued);
        if self.syncing {
            out.extend(self.create_progress());
        }
        out
    }

    pub fn sync_started(&mut self) -> Vec<Value> {
        self.syncing = true;
        self.phase = None;
        self.create_progress().into_iter().collect()
    }

    pub fn phase(&mut self, phase: &str) -> Vec<Value> {
        self.phase = Some(phase.to_string());
        match &self.progress {
            ProgressState::Active { token } => {
                vec![progress(token, json!({"kind": "report", "message": phase}))]
            }
            _ => vec![],
        }
    }

    /// The client's reply to one of the proxy's requests.
    pub fn response(&mut self, id: &str, ok: bool) -> Vec<Value> {
        let ProgressState::Creating { id: pending, token } = &self.progress else {
            return vec![];
        };
        if pending != id {
            return vec![];
        }
        let token = token.clone();
        if !(ok && self.syncing) {
            self.progress = ProgressState::Idle;
            return vec![];
        }
        let mut value = json!({"kind": "begin", "title": TITLE, "cancellable": false});
        if let Some(phase) = &self.phase {
            value["message"] = json!(phase);
        }
        self.progress = ProgressState::Active {
            token: token.clone(),
        };
        vec![progress(&token, value)]
    }

    pub fn sync_done(&mut self, result: &Result<SyncOutcome, SyncError>) -> Vec<Value> {
        self.syncing = false;
        let mut out = Vec::new();
        if let ProgressState::Active { token } = &self.progress {
            out.push(progress(
                token,
                json!({"kind": "end", "message": end_message(result)}),
            ));
            self.progress = ProgressState::Idle;
        }
        if let Some(message) = show_message(result) {
            if self.ready {
                out.push(message);
            } else {
                self.queued.push(message);
            }
        }
        out
    }

    fn create_progress(&mut self) -> Option<Value> {
        if !(self.ready && self.work_done_progress && matches!(self.progress, ProgressState::Idle))
        {
            return None;
        }
        self.requests += 1;
        let id = format!("bkl-{}", self.requests);
        let token = format!("bkl-sync-{}", self.requests);
        let msg = json!({
            "jsonrpc": "2.0", "id": id, "method": "window/workDoneProgress/create",
            "params": {"token": token}
        });
        self.progress = ProgressState::Creating { id, token };
        Some(msg)
    }
}

fn progress(token: &str, value: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": "$/progress", "params": {"token": token, "value": value}})
}

fn end_message(result: &Result<SyncOutcome, SyncError>) -> String {
    match result {
        Err(_) => "failed".into(),
        Ok(o) => {
            let s = &o.summary;
            let counts = format!("{} modules, {} libraries", s.modules, s.libraries);
            if s.status == "ok" {
                counts
            } else {
                format!("{counts} (incomplete)")
            }
        }
    }
}

fn show_message(result: &Result<SyncOutcome, SyncError>) -> Option<Value> {
    let (kind, text) = match result {
        Err(e) => (ERROR, describe("Bazel sync failed", e)),
        Ok(o) if o.summary.status != "ok" => {
            let headline = "Bazel sync incomplete, navigation may miss some targets";
            let text = match &o.summary.partial_reason {
                Some(reason) => describe(headline, reason),
                None => format!("{headline}\nLog: {}", o.summary.log.display()),
            };
            (WARNING, text)
        }
        Ok(_) => return None,
    };
    Some(json!({
        "jsonrpc": "2.0", "method": "window/showMessage",
        "params": {"type": kind, "message": text}
    }))
}

pub fn describe(headline: &str, e: &SyncError) -> String {
    let mut lines = vec![format!("{headline}: {}", e.message)];
    lines.extend(highlights(&e.stderr_tail).into_iter().map(str::to_string));
    if let Some(command) = &e.command {
        lines.push(format!("Reproduce: {command}"));
    }
    if let Some(log) = &e.log {
        lines.push(format!("Log: {}", log.display()));
    }
    lines.join("\n")
}

/// Bazel's own error lines from the end of its stderr, else its last lines.
fn highlights(tail: &[String]) -> Vec<&str> {
    let errors: Vec<&str> = tail
        .iter()
        .map(String::as_str)
        .filter(|l| l.starts_with("ERROR:") || l.starts_with("FATAL:"))
        .filter(|l| !l.contains("Build did NOT complete successfully"))
        .collect();
    let lines = if errors.is_empty() {
        tail.iter().map(String::as_str).collect()
    } else {
        errors
    };
    lines[lines.len().saturating_sub(SHOWN_LINES)..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn highlights_prefer_error_lines() {
        let tail = lines(&[
            "ERROR: a",
            "INFO: x",
            "FATAL: b",
            "ERROR: Build did NOT complete successfully",
        ]);
        assert_eq!(highlights(&tail), ["ERROR: a", "FATAL: b"]);
    }

    #[test]
    fn highlights_fall_back_to_the_last_lines() {
        let tail = lines(&["1", "2", "3", "4", "5", "6", "7"]);
        assert_eq!(highlights(&tail), ["3", "4", "5", "6", "7"]);
    }

    #[test]
    fn nothing_is_sent_before_initialized() {
        let mut r = Reporter::default();
        r.initialize(true);
        assert!(r.sync_started().is_empty());
        assert!(r.sync_done(&Err(SyncError::default())).is_empty());
        let out = r.initialized();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["method"], "window/showMessage");
    }

    #[test]
    fn a_late_creation_reply_starts_no_progress() {
        let mut r = Reporter::default();
        r.initialize(true);
        r.initialized();
        let create = r.sync_started();
        let id = create[0]["id"].as_str().unwrap().to_string();
        r.sync_done(&Err(SyncError::default()));
        assert!(r.response(&id, true).is_empty());
        assert_eq!(
            r.sync_started()[0]["method"],
            "window/workDoneProgress/create"
        );
    }
}
