//! LSP base protocol framing: `Content-Length` headers around JSON-RPC bodies.

use std::io::{self, BufRead, Write};

use serde_json::Value;

/// Reads one message. `Ok(None)` at a clean end of stream (before any header).
pub fn read_message(r: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut length = None;
    let mut saw_header = false;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line)? == 0 {
            return if saw_header {
                Err(io::ErrorKind::UnexpectedEof.into())
            } else {
                Ok(None)
            };
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if saw_header {
                break;
            }
            continue;
        }
        saw_header = true;
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
            );
        }
    }
    let length = length
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length"))?;
    let mut body = vec![0; length];
    r.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Writes one message and flushes.
pub fn write_message(w: &mut impl Write, msg: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(msg)?;
    write!(w, "Content-Length: {}\r\n\r\n", body.len())?;
    w.write_all(&body)?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    fn framed(body: &str) -> Vec<u8> {
        format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
    }

    #[test]
    fn round_trip() {
        let msg = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"x": "é"}});
        let mut buf = Vec::new();
        write_message(&mut buf, &msg).unwrap();
        assert_eq!(read_message(&mut Cursor::new(buf)).unwrap(), Some(msg));
    }

    #[test]
    fn two_messages_back_to_back() {
        let mut bytes = framed(r#"{"id":1}"#);
        bytes.extend(framed(r#"{"id":2}"#));
        let mut r = Cursor::new(bytes);
        assert_eq!(read_message(&mut r).unwrap(), Some(json!({"id": 1})));
        assert_eq!(read_message(&mut r).unwrap(), Some(json!({"id": 2})));
        assert_eq!(read_message(&mut r).unwrap(), None);
    }

    #[test]
    fn eof_returns_none() {
        assert_eq!(read_message(&mut Cursor::new(Vec::new())).unwrap(), None);
    }

    #[test]
    fn header_case_insensitive_and_content_type_ignored() {
        let body = r#"{"id":3}"#;
        let bytes = format!(
            "content-length: {}\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n{body}",
            body.len()
        );
        assert_eq!(
            read_message(&mut Cursor::new(bytes)).unwrap(),
            Some(json!({"id": 3}))
        );
    }

    #[test]
    fn truncated_body_is_error() {
        let bytes = b"Content-Length: 50\r\n\r\n{\"id\":1}".to_vec();
        assert!(read_message(&mut Cursor::new(bytes)).is_err());
    }

    #[test]
    fn missing_length_is_error() {
        assert!(read_message(&mut Cursor::new(b"X: 1\r\n\r\n{}".to_vec())).is_err());
    }
}
