//! What the proxy tells the editor about syncs: `$/progress` and `window/showMessageRequest`.
//!
//! Nothing is sent before the client's `initialized` notification, and progress only to clients that advertise
//! `window.workDoneProgress`. A progress token is used only once the client has answered its creation request.
//! Failures are prompts with a button, not `window/showMessage`: Zed dismisses button-less messages after 5 s.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::sync::{SyncError, SyncOutcome};

const TITLE: &str = "Bazel sync";
const SHOWN_LINES: usize = 5;
const ERROR: u8 = 1;
const WARNING: u8 = 2;
const OPEN_LOG: &str = "Open log";
const OK: &str = "OK";

#[derive(Default)]
pub struct Reporter {
    ready: bool,
    work_done_progress: bool,
    show_document: bool,
    /// Unanswered prompts, by request id, with the log their "Open log" opens.
    prompts: BTreeMap<String, Option<PathBuf>>,
    /// The last prompt shown since the last successful sync.
    last_prompt: Option<String>,
    syncing: bool,
    phase: Option<String>,
    requests: u64,
    progress: ProgressState,
    /// Prompts for syncs that ended before `initialized`, built once the client's capabilities are known.
    queued: Vec<Prompt>,
}

/// (message type, Markdown text, log).
type Prompt = (u8, String, Option<PathBuf>);

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
    pub fn initialize(&mut self, work_done_progress: bool, show_document: bool) {
        self.work_done_progress = work_done_progress;
        self.show_document = show_document;
    }

    pub fn initialized(&mut self) -> Vec<Value> {
        self.ready = true;
        let mut out: Vec<Value> = std::mem::take(&mut self.queued)
            .into_iter()
            .map(|p| self.show(p))
            .collect();
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
    pub fn response(&mut self, id: &str, ok: bool, result: Option<&Value>) -> Vec<Value> {
        if let Some(log) = self.prompts.remove(id) {
            let chose_open =
                result.and_then(|r| r.get("title")).and_then(Value::as_str) == Some(OPEN_LOG);
            return match (chose_open, log) {
                (true, Some(log)) => vec![self.request(
                    "window/showDocument",
                    json!({"uri": super::file_uri(&log), "takeFocus": true}),
                )],
                _ => vec![],
            };
        }
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
        if matches!(result, Ok(o) if o.summary.status == "ok") {
            self.last_prompt = None;
        }
        if let Some(prompt) = prompt(result)
            && self.last_prompt.as_ref() != Some(&prompt.1)
        {
            self.last_prompt = Some(prompt.1.clone());
            if self.ready {
                out.push(self.show(prompt));
            } else {
                self.queued.push(prompt);
            }
        }
        out
    }

    fn show(&mut self, (kind, text, log): Prompt) -> Value {
        let action = if self.show_document && log.is_some() {
            OPEN_LOG
        } else {
            OK
        };
        let message = self.request(
            "window/showMessageRequest",
            json!({"type": kind, "message": text, "actions": [{"title": action}]}),
        );
        let id = message["id"].as_str().unwrap_or_default().to_string();
        self.prompts.insert(id, log);
        message
    }

    fn next_id(&mut self) -> u64 {
        self.requests += 1;
        self.requests
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = format!("bkl-{}", self.next_id());
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    }

    fn create_progress(&mut self) -> Option<Value> {
        if !(self.ready && self.work_done_progress && matches!(self.progress, ProgressState::Idle))
        {
            return None;
        }
        let n = self.next_id();
        let id = format!("bkl-{n}");
        let token = format!("bkl-sync-{n}");
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

/// (message type, Markdown text, log) of the prompt for a sync result, if it needs one.
fn prompt(result: &Result<SyncOutcome, SyncError>) -> Option<Prompt> {
    match result {
        Err(e) => Some((ERROR, markdown("Bazel sync failed", e), e.log.clone())),
        Ok(o) if o.summary.status != "ok" => {
            let headline = "Bazel sync incomplete, navigation may miss some targets";
            Some(match &o.summary.partial_reason {
                Some(reason) => (WARNING, markdown(headline, reason), reason.log.clone()),
                None => (
                    WARNING,
                    format!("**{headline}**\n\nLog: `{}`", o.summary.log.display()),
                    Some(o.summary.log.clone()),
                ),
            })
        }
        Ok(_) => None,
    }
}

/// Zed renders prompt text as Markdown.
fn markdown(headline: &str, e: &SyncError) -> String {
    let mut parts = vec![format!("**{headline}:** {}", e.message)];
    let lines = highlights(&e.stderr_tail);
    if !lines.is_empty() {
        parts.push(format!("```\n{}\n```", lines.join("\n")));
    }
    if let Some(command) = &e.command {
        parts.push(format!("Reproduce:\n```\n{command}\n```"));
    }
    if let Some(log) = &e.log {
        parts.push(format!("Log: `{}`", log.display()));
    }
    parts.join("\n\n")
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
        r.initialize(true, false);
        assert!(r.sync_started().is_empty());
        assert!(r.sync_done(&Err(SyncError::default())).is_empty());
        let out = r.initialized();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["method"], "window/showMessageRequest");
    }

    fn failure(message: &str) -> Result<SyncOutcome, SyncError> {
        Err(SyncError {
            message: message.into(),
            command: Some("bazelisk build //...".into()),
            stderr_tail: vec!["ERROR: /ws/b/BUILD.bazel:1:1: no such target".into()],
            log: Some("/c/sync.log".into()),
        })
    }

    fn ok() -> Result<SyncOutcome, SyncError> {
        let mut outcome = SyncOutcome::default();
        outcome.summary.status = "ok".into();
        Ok(outcome)
    }

    fn ready(show_document: bool) -> Reporter {
        let mut r = Reporter::default();
        r.initialize(false, show_document);
        r.initialized();
        r
    }

    #[test]
    fn a_failure_is_a_prompt_that_stays_until_answered() {
        let mut r = ready(true);
        let out = r.sync_done(&failure("Bazel build failed (exit 2)"));
        assert_eq!(out.len(), 1);
        let prompt = &out[0];
        assert_eq!(prompt["method"], "window/showMessageRequest", "{prompt}");
        assert!(
            prompt["id"].as_str().unwrap().starts_with("bkl-"),
            "{prompt}"
        );
        assert_eq!(prompt["params"]["type"], ERROR);
        assert_eq!(prompt["params"]["actions"], json!([{"title": "Open log"}]));
        let text = prompt["params"]["message"].as_str().unwrap();
        for expected in [
            "Bazel build failed (exit 2)",
            "ERROR: /ws/b/BUILD.bazel:1:1: no such target",
            "bazelisk build //...",
            "/c/sync.log",
        ] {
            assert!(text.contains(expected), "{expected:?} missing from {text}");
        }
    }

    #[test]
    fn open_log_shows_the_log_file() {
        let mut r = ready(true);
        let prompt = r.sync_done(&failure("boom")).remove(0);
        let id = prompt["id"].as_str().unwrap();
        let out = r.response(id, true, Some(&json!({"title": "Open log"})));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["method"], "window/showDocument", "{}", out[0]);
        assert_eq!(out[0]["params"]["uri"], "file:///c/sync.log");
        assert!(out[0]["id"].as_str().unwrap().starts_with("bkl-"));
        assert!(
            r.response(id, true, None).is_empty(),
            "a prompt is answered once"
        );
    }

    #[test]
    fn closing_the_prompt_opens_nothing() {
        let mut r = ready(true);
        let prompt = r.sync_done(&failure("boom")).remove(0);
        let id = prompt["id"].as_str().unwrap();
        assert!(r.response(id, true, Some(&Value::Null)).is_empty());
    }

    #[test]
    fn without_show_document_the_prompt_offers_ok() {
        let mut r = ready(false);
        let prompt = r.sync_done(&failure("boom")).remove(0);
        assert_eq!(prompt["params"]["actions"], json!([{"title": "OK"}]));
        let id = prompt["id"].as_str().unwrap();
        assert!(
            r.response(id, true, Some(&json!({"title": "OK"})))
                .is_empty()
        );
    }

    #[test]
    fn the_same_failure_is_prompted_once_until_a_sync_succeeds() {
        let mut r = ready(true);
        assert_eq!(r.sync_done(&failure("boom")).len(), 1);
        assert!(
            r.sync_done(&failure("boom")).is_empty(),
            "same failure again"
        );
        assert_eq!(r.sync_done(&failure("other")).len(), 1, "a new failure");
        assert!(r.sync_done(&ok()).is_empty());
        assert_eq!(
            r.sync_done(&failure("other")).len(),
            1,
            "failing again after a success"
        );
    }

    #[test]
    fn a_late_creation_reply_starts_no_progress() {
        let mut r = Reporter::default();
        r.initialize(true, false);
        r.initialized();
        let create = r.sync_started();
        let id = create[0]["id"].as_str().unwrap().to_string();
        r.sync_done(&Err(SyncError::default()));
        assert!(r.response(&id, true, None).is_empty());
        assert_eq!(
            r.sync_started()[0]["method"],
            "window/workDoneProgress/create"
        );
    }
}
