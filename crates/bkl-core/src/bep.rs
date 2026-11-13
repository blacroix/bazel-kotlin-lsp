//! Build Event Protocol (`--build_event_json_file`) reading.

use std::path::PathBuf;

use anyhow::Context;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct BepFiles {
    pub local: Vec<PathBuf>,
    /// URIs of aspect outputs that are not plain local files (e.g. left in a remote cache).
    pub not_local: Vec<String>,
}

/// The `.bkl.json` aspect outputs announced in a newline-delimited BEP JSON file.
pub fn aspect_json_files(bep_ndjson: &str) -> anyhow::Result<BepFiles> {
    let mut out = BepFiles::default();
    for (n, line) in bep_ndjson
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
    {
        let event: serde_json::Value =
            serde_json::from_str(line).with_context(|| format!("BEP line {}", n + 1))?;
        let Some(files) = event
            .pointer("/namedSetOfFiles/files")
            .and_then(|f| f.as_array())
        else {
            continue;
        };
        for file in files {
            let name = file
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if !name.ends_with(".bkl.json") {
                continue;
            }
            let uri = file.get("uri").and_then(|v| v.as_str()).unwrap_or_default();
            match url::Url::parse(uri)
                .ok()
                .filter(|u| u.scheme() == "file")
                .and_then(|u| u.to_file_path().ok())
            {
                Some(path) => out.local.push(path),
                None => out.not_local.push(uri.to_string()),
            }
        }
    }
    out.local.sort();
    out.local.dedup();
    out.not_local.sort();
    out.not_local.dedup();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bep_collects_local_and_remote() {
        let bep = [
            r#"{"id":{"namedSet":{"id":"0"}},"namedSetOfFiles":{"files":[{"name":"x.bkl.json","uri":"file:///tmp/a%20b/x.bkl.json"}]}}"#,
            r#"{"id":{"namedSet":{"id":"1"}},"namedSetOfFiles":{"files":[{"name":"y.bkl.json","uri":"bytestream://cache/blobs/abc/12"},{"name":"app.jar","uri":"file:///tmp/app.jar"}]}}"#,
            r#"{"id":{"progress":{}},"progress":{}}"#,
        ]
        .join("\n");
        let files = aspect_json_files(&bep).unwrap();
        assert_eq!(files.local, [PathBuf::from("/tmp/a b/x.bkl.json")]);
        assert_eq!(files.not_local, ["bytestream://cache/blobs/abc/12"]);
    }
}
