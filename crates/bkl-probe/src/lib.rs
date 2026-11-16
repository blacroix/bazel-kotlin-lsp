//! Headless LSP probe: asks go-to-definition (and diagnostics) on known symbols and reports PASS/FAIL per case.

pub mod cases;
pub mod client;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context;
use serde_json::{Value, json};

use cases::{Case, CaseKind, position_of};
use client::Client;

const POLL: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone)]
pub struct ProbeArgs {
    pub root: PathBuf,
    pub cases: PathBuf,
    pub model_dir: PathBuf,
    pub index_dir: Option<PathBuf>,
    pub timeout: Duration,
    pub log: Option<PathBuf>,
    pub server: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseResult {
    pub name: String,
    pub pass: bool,
    pub targets: Vec<String>,
}

pub fn run(args: &ProbeArgs) -> anyhow::Result<Vec<CaseResult>> {
    let root = std::path::absolute(&args.root)?;
    let cases: Vec<Case> = serde_json::from_slice(
        &std::fs::read(&args.cases).with_context(|| format!("{}", args.cases.display()))?,
    )
    .with_context(|| format!("parsing {}", args.cases.display()))?;
    let kinds = cases
        .iter()
        .map(Case::kind)
        .collect::<anyhow::Result<Vec<_>>>()?;

    let client = Client::spawn(&args.server, args.log.as_deref())?;
    let mut options = json!({"projects": [{"type": "json", "path": file_uri(&std::path::absolute(&args.model_dir)?)}]});
    if let Some(index) = &args.index_dir {
        options["indexDir"] = json!(index);
    }
    let init = client.request(
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": file_uri(&root),
            "workspaceFolders": [{"uri": file_uri(&root), "name": root.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()}],
            "initializationOptions": options,
            "capabilities": {
                "workspace": {"configuration": true, "workspaceFolders": true},
                "window": {"workDoneProgress": true},
                "textDocument": {"definition": {"linkSupport": false}, "publishDiagnostics": {}, "synchronization": {}}
            }
        }),
        Duration::from_secs(300),
    )?;
    if let Some(error) = init.get("error") {
        anyhow::bail!("initialize failed: {error}");
    }
    client.notify("initialized", json!({}))?;
    client.notify("workspace/didChangeConfiguration", json!({"settings": {}}))?;

    let mut opened = HashSet::new();
    let mut results = Vec::new();
    for (case, kind) in cases.iter().zip(kinds) {
        let path = root.join(&case.file);
        let text = std::fs::read_to_string(&path).with_context(|| format!("{}", path.display()))?;
        let uri = file_uri(&path);
        if opened.insert(uri.clone()) {
            let language = if case.file.ends_with(".java") {
                "java"
            } else {
                "kotlin"
            };
            client.notify("textDocument/didOpen", json!({"textDocument": {"uri": uri, "languageId": language, "version": 1, "text": text}}))?;
        }
        let deadline = Instant::now() + args.timeout;
        let result = match kind {
            CaseKind::Definition {
                line_contains,
                symbol,
                expect,
            } => {
                let (line, character) =
                    position_of(&text, &line_contains, &symbol).with_context(|| {
                        format!(
                            "case {:?}: {line_contains:?} / {symbol:?} not found in {}",
                            case.name, case.file
                        )
                    })?;
                definition_case(&client, &uri, line, character, &expect, deadline)
            }
            CaseKind::NoErrors => no_errors_case(&client, &uri, deadline),
        };
        results.push(CaseResult {
            name: case.name.clone(),
            pass: result.0,
            targets: result.1,
        });
    }
    client.shutdown();
    Ok(results)
}

/// Asks for the definition every few seconds until a target contains `expect` or `deadline` passes; returns
/// whether it did and the last targets. A deadline in the past asks once.
pub fn definition_case(
    client: &Client,
    uri: &str,
    line: u32,
    character: u32,
    expect: &str,
    deadline: Instant,
) -> (bool, Vec<String>) {
    loop {
        let params = json!({"textDocument": {"uri": uri}, "position": {"line": line, "character": character}});
        let targets = match client.request("textDocument/definition", params, REQUEST_TIMEOUT) {
            Ok(resp) if resp.get("error").is_some() => vec![format!("error: {}", resp["error"])],
            Ok(resp) => locations(resp.get("result").unwrap_or(&Value::Null)),
            Err(e) => vec![format!("error: {e}")],
        };
        if targets.iter().any(|t| t.contains(expect)) {
            return (true, targets);
        }
        if Instant::now() + POLL > deadline {
            return (false, targets);
        }
        std::thread::sleep(POLL);
    }
}

/// Judged on pushed diagnostics only: kotlin-lsp answers pull requests with empty reports until it has analysed
/// the file, but pushes once analysis is done.
fn no_errors_case(client: &Client, uri: &str, deadline: Instant) -> (bool, Vec<String>) {
    let mut errors = vec!["no diagnostics published".to_string()];
    loop {
        if let Some(diagnostics) = client.published_diagnostics(uri) {
            errors = diagnostics
                .iter()
                .filter(|d| d.get("severity").and_then(Value::as_i64) == Some(1))
                .map(|d| {
                    d.get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect();
            if errors.is_empty() {
                return (true, vec![]);
            }
        }
        if Instant::now() + POLL > deadline {
            return (false, errors);
        }
        std::thread::sleep(POLL);
    }
}

fn locations(result: &Value) -> Vec<String> {
    let items = match result {
        Value::Array(items) => items.clone(),
        Value::Null => vec![],
        single => vec![single.clone()],
    };
    items
        .iter()
        .filter_map(|l| {
            l.get("uri")
                .or_else(|| l.get("targetUri"))
                .and_then(Value::as_str)
        })
        .map(percent_decode)
        .collect()
}

pub fn file_uri(path: &Path) -> String {
    url::Url::from_file_path(path)
        .map(String::from)
        .unwrap_or_else(|_| format!("file://{}", path.display()))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_accept_location_links_and_single_locations() {
        let links = json!([{"targetUri": "jar:///a%20b/x-sources.jar!/X.kt"}]);
        assert_eq!(locations(&links), ["jar:///a b/x-sources.jar!/X.kt"]);
        assert_eq!(
            locations(&json!({"uri": "file:///x/Y.kt"})),
            ["file:///x/Y.kt"]
        );
        assert!(locations(&Value::Null).is_empty());
    }
}
