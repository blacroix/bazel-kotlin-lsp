//! Library files through the whole proxy: a child that names a `jar:` file and serves its text.

use std::io::BufReader;
use std::sync::Arc;
use std::sync::mpsc::channel;
use std::time::Duration;

use bazel_kotlin_lsp::bazel::Cancel;
use bazel_kotlin_lsp::proxy::{BazelMode, Mode, Syncer, serve};
use bazel_kotlin_lsp::sync::{Progress, SyncError, SyncOutcome};
use bkl_lsp::{read_message, write_message};
use serde_json::{Value, json};

struct NeverSyncs;

impl Syncer for NeverSyncs {
    fn sync(&self, cancel: &Cancel, _: &Progress) -> Result<SyncOutcome, SyncError> {
        while !cancel.is_cancelled() {
            std::thread::sleep(Duration::from_millis(20));
        }
        Err(SyncError::default())
    }
}

fn framed(msg: &Value) -> String {
    let body = msg.to_string();
    format!("Content-Length: {}\r\n\r\n{body}", body.len())
}

const JAR: &str = "jar:///libs/x-sources.jar!/pkg/A.kt";

#[test]
fn the_editor_opens_library_files_through_file_addresses() {
    let t = tempfile::tempdir().unwrap();
    let received = t.path().join("received");
    let library = t.path().join("cache/library-sources");
    let definition = json!({"jsonrpc": "2.0", "id": 2, "result": [{"uri": JAR, "range": {}}]});
    let content =
        json!({"jsonrpc": "2.0", "id": "bkl-content-1", "result": {"text": "package pkg\n"}});
    let script = r#"exec 3<&0; printf '%s' "$1"; cat > "$0" <&3 &
i=0; while ! grep -q textDocumentContent "$0" 2>/dev/null && [ $i -lt 200 ]; do sleep 0.05; i=$((i+1)); done
printf '%s' "$2"; wait"#;
    let command: Vec<String> = [
        "sh",
        "-c",
        script,
        received.to_str().unwrap(),
        &framed(&definition),
        &framed(&content),
    ]
    .map(String::from)
    .into();
    let mode = Mode::Bazel(
        BazelMode::new(t.path().join("model"), Arc::new(NeverSyncs)).with_library_dir(library),
    );
    let (client_r, mut to_proxy) = std::io::pipe().unwrap();
    let (proxy_r, client_w) = std::io::pipe().unwrap();
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
            let _ = msg_tx.send(msg);
        }
    });

    let reply = from_proxy.recv_timeout(Duration::from_secs(10)).unwrap();
    let uri = reply["result"][0]["uri"].as_str().unwrap().to_string();
    let path = url::Url::parse(&uri).unwrap().to_file_path().unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "package pkg\n");

    let open = json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
        "textDocument": {"uri": uri, "languageId": "kotlin", "version": 1, "text": "package pkg\n"}
    }});
    write_message(&mut to_proxy, &open).unwrap();
    drop(to_proxy);
    assert_eq!(
        done.recv_timeout(Duration::from_secs(20)).unwrap().unwrap(),
        0
    );

    let bytes = std::fs::read(&received).unwrap();
    let mut r = std::io::Cursor::new(bytes);
    let mut child_got = Vec::new();
    while let Some(msg) = read_message(&mut r).unwrap() {
        child_got.push(msg);
    }
    assert_eq!(child_got[0]["method"], "workspace/textDocumentContent");
    assert_eq!(child_got[0]["params"]["uri"], JAR);
    assert_eq!(child_got[1]["params"]["textDocument"]["uri"], JAR);
}
