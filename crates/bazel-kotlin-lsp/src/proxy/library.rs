//! Library files for editors that only open `file:` addresses.
//!
//! kotlin-lsp names library files with `jar:` (and `jrt:`) addresses. The proxy asks kotlin-lsp for their text,
//! writes it read-only under the workspace cache, hands the editor that file, and maps it back on the way in.

use std::collections::{HashMap, HashSet};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{file_uri, log};

/// How long a message waits for the text of the library files it names.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// Library files fetched for one message at most; a long symbol list should not decompile half the classpath.
const MAX_FETCHES: usize = 32;
const SCHEMES: [&str; 2] = ["jar", "jrt"];
const REQUEST_ID: &str = "bkl-content-";
const KOTLIN_STUB: &str = "// IntelliJ API Decompiler stub";
const LANGUAGE_SUFFIXES: [&str; 2] = ["kt", "java"];

/// Maps library addresses to files under one folder, and back.
pub struct Library {
    root: PathBuf,
}

impl Library {
    pub fn new(root: PathBuf) -> Self {
        Library { root }
    }

    /// The file holding `uri`'s text, if it is written and not older than its archive.
    pub fn fresh(&self, uri: &str) -> Option<PathBuf> {
        let base = self.base_path(uri)?;
        let archive = archive(uri).and_then(|a| a.metadata().ok()?.modified().ok());
        candidates(&base).into_iter().find(|path| {
            path.metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|written| archive.is_none_or(|a| a <= written))
        })
    }

    /// Writes `uri`'s text, read-only; returns the file.
    pub fn write(&self, uri: &str, text: &str) -> io::Result<PathBuf> {
        let base = self.base_path(uri).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("not a library address: {uri}"),
            )
        })?;
        let path = if is_class(&base) {
            let language = if text.starts_with(KOTLIN_STUB) {
                "kt"
            } else {
                "java"
            };
            with_suffix(&base, language)
        } else {
            base
        };
        let dir = path.parent().expect("under the root");
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(".tmp-{}", std::process::id()));
        std::fs::write(&tmp, text)?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o444))?;
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }

    /// The library address a `file:` address under the folder stands for.
    pub fn remote_uri(&self, file_uri: &str) -> Option<String> {
        let path = url::Url::parse(file_uri).ok()?.to_file_path().ok()?;
        let rel = path.strip_prefix(&self.root).ok()?.to_str()?;
        let (scheme, rest) = rel.split_once('/')?;
        if !SCHEMES.contains(&scheme) {
            return None;
        }
        let rest = LANGUAGE_SUFFIXES
            .iter()
            .find_map(|suffix| rest.strip_suffix(&format!(".class.{suffix}")))
            .map_or(rest.to_string(), |stem| format!("{stem}.class"));
        Some(format!("{scheme}:///{rest}"))
    }

    /// An editor message with the folder's files named by their library addresses.
    pub fn to_server(&self, body: Vec<u8>) -> Vec<u8> {
        let marker = self
            .root
            .file_name()
            .map(|n| n.as_encoded_bytes())
            .unwrap_or_default();
        if marker.is_empty() || !contains(&body, marker) {
            return body;
        }
        let Ok(mut msg) = serde_json::from_slice::<Value>(&body) else {
            return body;
        };
        let changed = map_strings(&mut msg, &mut |s| {
            s.starts_with("file:").then(|| self.remote_uri(s)).flatten()
        });
        if changed {
            serde_json::to_vec(&msg).unwrap_or(body)
        } else {
            body
        }
    }

    /// Where `uri`'s text goes, before any language suffix; `None` unless the path maps back to exactly `uri`.
    fn base_path(&self, uri: &str) -> Option<PathBuf> {
        let (scheme, rest) = uri.split_once(":///")?;
        if !SCHEMES.contains(&scheme) || rest.is_empty() {
            return None;
        }
        let rel = Path::new(rest);
        let normal = rel.components().all(|c| matches!(c, Component::Normal(_)));
        let rebuilt: PathBuf = rel.components().collect();
        (normal && rebuilt.as_os_str() == rel.as_os_str()).then(|| self.root.join(scheme).join(rel))
    }

    /// Library addresses in `msg` that map to a file.
    fn addresses(&self, msg: &Value) -> Vec<String> {
        let mut found = Vec::new();
        visit_strings(msg, &mut |s| {
            if self.base_path(s).is_some() && !found.iter().any(|f| f == s) {
                found.push(s.to_string());
            }
        });
        found
    }

    /// `msg` with each written library file named by its `file:` address.
    fn rewrite(&self, mut msg: Value) -> Vec<u8> {
        map_strings(&mut msg, &mut |s| {
            self.base_path(s)
                .and_then(|_| self.fresh(s))
                .map(|path| file_uri(&path))
        });
        serde_json::to_vec(&msg).expect("serializable")
    }
}

fn is_class(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "class")
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".");
    name.push(suffix);
    name.into()
}

fn candidates(base: &Path) -> Vec<PathBuf> {
    if is_class(base) {
        LANGUAGE_SUFFIXES
            .iter()
            .map(|s| with_suffix(base, s))
            .collect()
    } else {
        vec![base.to_path_buf()]
    }
}

/// The local archive a `jar:` address points into.
fn archive(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("jar://")?;
    let (archive, _) = rest.split_once("!/")?;
    url::Url::parse(&format!("file://{archive}"))
        .ok()?
        .to_file_path()
        .ok()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn visit_strings(value: &Value, f: &mut impl FnMut(&str)) {
    match value {
        Value::String(s) => f(s),
        Value::Array(items) => items.iter().for_each(|v| visit_strings(v, f)),
        Value::Object(map) => map.values().for_each(|v| visit_strings(v, f)),
        _ => {}
    }
}

/// Replaces each string `f` maps; returns whether any was.
fn map_strings(value: &mut Value, f: &mut impl FnMut(&str) -> Option<String>) -> bool {
    match value {
        Value::String(s) => match f(s) {
            Some(new) => {
                *s = new;
                true
            }
            None => false,
        },
        Value::Array(items) => items
            .iter_mut()
            .fold(false, |changed, v| map_strings(v, f) | changed),
        Value::Object(map) => map
            .values_mut()
            .fold(false, |changed, v| map_strings(v, f) | changed),
        _ => false,
    }
}

pub enum Out {
    Client(Vec<u8>),
    Child(Vec<u8>),
}

/// Holds kotlin-lsp's messages that name library files until their text is written.
pub struct Relay {
    library: Arc<Library>,
    next_id: u64,
    /// The proxy's content requests by id, with the address each asks for.
    requests: HashMap<String, String>,
    fetching: HashSet<String>,
    parked: Vec<(Value, Instant)>,
}

impl Relay {
    pub fn new(library: Arc<Library>) -> Self {
        Relay {
            library,
            next_id: 0,
            requests: HashMap::new(),
            fetching: HashSet::new(),
            parked: Vec::new(),
        }
    }

    /// What to send for one kotlin-lsp message.
    pub fn child_message(&mut self, body: Vec<u8>, now: Instant) -> Vec<Out> {
        if !SCHEMES
            .iter()
            .any(|s| contains(&body, format!("\"{s}:").as_bytes()))
            && !contains(&body, REQUEST_ID.as_bytes())
        {
            return vec![Out::Client(body)];
        }
        let Ok(msg) = serde_json::from_slice::<Value>(&body) else {
            return vec![Out::Client(body)];
        };
        if msg.get("method").is_none()
            && let Some(uri) = msg
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| self.requests.remove(id))
        {
            self.fetched(&uri, &msg);
            return self.release(now, false);
        }
        let missing: Vec<String> = self
            .library
            .addresses(&msg)
            .into_iter()
            .filter(|uri| self.library.fresh(uri).is_none())
            .take(MAX_FETCHES)
            .collect();
        if missing.is_empty() {
            return vec![Out::Client(self.library.rewrite(msg))];
        }
        let mut out = Vec::new();
        for uri in missing {
            if self.fetching.insert(uri.clone()) {
                out.push(self.request(uri));
            }
        }
        self.parked.push((msg, now));
        out
    }

    /// Sends the messages that waited longer than [`FETCH_TIMEOUT`], or all of them with `all`.
    pub fn expire(&mut self, now: Instant, all: bool) -> Vec<Out> {
        self.release(now, all)
    }

    fn request(&mut self, uri: String) -> Out {
        self.next_id += 1;
        let id = format!("{REQUEST_ID}{}", self.next_id);
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": "workspace/textDocumentContent", "params": {"uri": uri}});
        self.requests.insert(id, uri);
        Out::Child(serde_json::to_vec(&msg).expect("serializable"))
    }

    fn fetched(&mut self, uri: &str, reply: &Value) {
        self.fetching.remove(uri);
        match reply.pointer("/result/text").and_then(Value::as_str) {
            Some(text) => {
                if let Err(e) = self.library.write(uri, text) {
                    log(&format!("writing the text of {uri}: {e}"));
                }
            }
            None => log(&format!(
                "kotlin-lsp gave no text for {uri}: {}",
                reply.get("error").unwrap_or(&Value::Null)
            )),
        }
    }

    /// Sends, in order, the parked messages no longer waiting for a fetch.
    fn release(&mut self, now: Instant, all: bool) -> Vec<Out> {
        let (ready, waiting) =
            std::mem::take(&mut self.parked)
                .into_iter()
                .partition(|(msg, since)| {
                    all || now.duration_since(*since) >= FETCH_TIMEOUT
                        || !self
                            .library
                            .addresses(msg)
                            .iter()
                            .any(|uri| self.fetching.contains(uri))
                });
        self.parked = waiting;
        ready
            .into_iter()
            .map(|(msg, _)| Out::Client(self.library.rewrite(msg)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JAR: &str = "jar:///libs/x-sources.jar!/pkg/A.kt";

    fn library() -> (tempfile::TempDir, Arc<Library>) {
        let t = tempfile::tempdir().unwrap();
        let library = Arc::new(Library::new(t.path().join("library-sources")));
        (t, library)
    }

    fn body(msg: Value) -> Vec<u8> {
        serde_json::to_vec(&msg).unwrap()
    }

    fn definition(uri: &str) -> Value {
        json!({"jsonrpc": "2.0", "id": 7, "result": [{"uri": uri, "range": {}}]})
    }

    fn split(out: Vec<Out>) -> (Vec<Value>, Vec<Value>) {
        let (mut client, mut child) = (vec![], vec![]);
        for o in out {
            match o {
                Out::Client(b) => client.push(serde_json::from_slice(&b).unwrap()),
                Out::Child(b) => child.push(serde_json::from_slice(&b).unwrap()),
            }
        }
        (client, child)
    }

    fn content_reply(request: &Value, text: &str) -> Vec<u8> {
        body(json!({"jsonrpc": "2.0", "id": request["id"], "result": {"text": text}}))
    }

    #[test]
    fn a_library_location_waits_for_its_text_then_names_a_read_only_file() {
        let (_t, library) = library();
        let mut relay = Relay::new(library.clone());
        let now = Instant::now();

        let (client, child) = split(relay.child_message(body(definition(JAR)), now));
        assert!(client.is_empty(), "{client:?}");
        assert_eq!(child.len(), 1);
        assert_eq!(child[0]["method"], "workspace/textDocumentContent");
        assert_eq!(child[0]["params"], json!({"uri": JAR}));

        let (client, child) =
            split(relay.child_message(content_reply(&child[0], "package pkg\n"), now));
        assert!(child.is_empty());
        let path = library.fresh(JAR).expect("written");
        assert_eq!(client, [definition(&file_uri(&path))]);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "package pkg\n");
        assert!(path.ends_with("pkg/A.kt"), "{path:?}");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o222, 0, "writable: {mode:o}");
    }

    #[test]
    fn a_written_file_is_named_at_once() {
        let (_t, library) = library();
        let path = library.write(JAR, "x").unwrap();
        let mut relay = Relay::new(library);
        let (client, child) = split(relay.child_message(body(definition(JAR)), Instant::now()));
        assert!(child.is_empty());
        assert_eq!(client, [definition(&file_uri(&path))]);
    }

    #[test]
    fn one_fetch_serves_every_message_waiting_for_it() {
        let (_t, library) = library();
        let mut relay = Relay::new(library.clone());
        let now = Instant::now();
        let (_, child) = split(relay.child_message(body(definition(JAR)), now));
        let mut second = definition(JAR);
        second["id"] = json!(8);
        let (client, again) = split(relay.child_message(body(second.clone()), now));
        assert!(client.is_empty() && again.is_empty());
        let (client, _) = split(relay.child_message(content_reply(&child[0], "x"), now));
        let uri = file_uri(&library.fresh(JAR).unwrap());
        second["result"][0]["uri"] = json!(uri);
        assert_eq!(client, [definition(&uri), second]);
    }

    #[test]
    fn other_messages_pass_byte_for_byte_while_one_waits() {
        let (_t, library) = library();
        let mut relay = Relay::new(library);
        let now = Instant::now();
        relay.child_message(body(definition(JAR)), now);
        let other = b"{ \"jsonrpc\":\"2.0\", \"method\": \"$/progress\" }".to_vec();
        let out = relay.child_message(other.clone(), now);
        assert!(matches!(&out[..], [Out::Client(b)] if *b == other));
    }

    #[test]
    fn a_failed_fetch_keeps_the_library_address() {
        let (_t, library) = library();
        let mut relay = Relay::new(library);
        let now = Instant::now();
        let (_, child) = split(relay.child_message(body(definition(JAR)), now));
        let error = json!({"jsonrpc": "2.0", "id": child[0]["id"], "error": {"code": -32603, "message": "no"}});
        let (client, _) = split(relay.child_message(body(error), now));
        assert_eq!(client, [definition(JAR)]);
    }

    #[test]
    fn an_unanswered_fetch_releases_the_message_after_the_timeout() {
        let (_t, library) = library();
        let mut relay = Relay::new(library);
        let now = Instant::now();
        relay.child_message(body(definition(JAR)), now);
        assert!(relay.expire(now + Duration::from_secs(1), false).is_empty());
        let (client, _) = split(relay.expire(now + FETCH_TIMEOUT, false));
        assert_eq!(client, [definition(JAR)]);
    }

    #[test]
    fn closing_releases_every_waiting_message() {
        let (_t, library) = library();
        let mut relay = Relay::new(library);
        let now = Instant::now();
        relay.child_message(body(definition(JAR)), now);
        let (client, _) = split(relay.expire(now, true));
        assert_eq!(client, [definition(JAR)]);
    }

    #[test]
    fn a_class_file_gets_the_extension_of_its_decompiled_language() {
        let (_t, library) = library();
        let class = "jar:///libs/x.jar!/pkg/A.class";
        let kotlin = library
            .write(
                class,
                "// IntelliJ API Decompiler stub source generated from a class file\n",
            )
            .unwrap();
        assert!(kotlin.ends_with("pkg/A.class.kt"), "{kotlin:?}");
        assert_eq!(library.fresh(class), Some(kotlin.clone()));
        assert_eq!(
            library.remote_uri(&file_uri(&kotlin)).as_deref(),
            Some(class)
        );

        let java = "jar:///libs/y.jar!/pkg/B.class";
        let path = library.write(java, "package pkg;\n").unwrap();
        assert!(path.ends_with("pkg/B.class.java"), "{path:?}");
    }

    #[test]
    fn an_archive_newer_than_its_file_is_fetched_again() {
        let t = tempfile::tempdir().unwrap();
        let jar = t.path().join("x-sources.jar");
        std::fs::write(&jar, "").unwrap();
        let library = Library::new(t.path().join("library-sources"));
        let uri = format!("jar://{}!/A.kt", jar.display());
        let path = library.write(&uri, "old").unwrap();
        assert!(library.fresh(&uri).is_some());
        let later = std::fs::metadata(&path).unwrap().modified().unwrap() + Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&jar)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert_eq!(library.fresh(&uri), None);
        library.write(&uri, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
    }

    #[test]
    fn addresses_map_back_exactly() {
        let (_t, library) = library();
        for uri in [
            JAR,
            "jar:///a%20b/x+y.jar!/p/Q.kt",
            "jrt:///modules/java.base/java/lang/String.class",
        ] {
            let path = library.write(uri, "x").unwrap();
            assert_eq!(library.remote_uri(&file_uri(&path)).as_deref(), Some(uri));
        }
    }

    #[test]
    fn addresses_that_would_not_map_back_are_left_alone() {
        let (_t, library) = library();
        for uri in [
            "jar:///a/../b.jar!/A.kt",
            "jar:///a//b.jar!/A.kt",
            "jar://host/b.jar!/A.kt",
            "jar:///",
            "https://example.org/A.kt",
        ] {
            assert!(library.write(uri, "x").is_err(), "{uri}");
            let mut relay = Relay::new(library.clone());
            let out = relay.child_message(body(definition(uri)), Instant::now());
            assert_eq!(split(out).0, [definition(uri)], "{uri}");
        }
    }

    #[test]
    fn editor_messages_name_library_files_by_their_library_address() {
        let (_t, library) = library();
        let path = library.write(JAR, "x").unwrap();
        let open = json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": {"uri": file_uri(&path), "languageId": "kotlin", "version": 1, "text": "x"}
        }});
        let sent: Value = serde_json::from_slice(&library.to_server(body(open.clone()))).unwrap();
        let mut expected = open;
        expected["params"]["textDocument"]["uri"] = json!(JAR);
        assert_eq!(sent, expected);
    }

    #[test]
    fn other_editor_messages_pass_byte_for_byte() {
        let (_t, library) = library();
        let msg = b"{ \"method\": \"textDocument/didOpen\", \"params\": {\"uri\": \"file:///src/A.kt\"} }".to_vec();
        assert_eq!(library.to_server(msg.clone()), msg);
    }
}
