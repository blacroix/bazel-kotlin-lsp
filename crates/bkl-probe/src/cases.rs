//! The probe's cases file: the schema of the original Python probe, plus `expect_no_errors`.

use anyhow::bail;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Case {
    pub name: String,
    pub file: String,
    #[serde(default)]
    pub line_contains: Option<String>,
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub expect: Option<String>,
    #[serde(default)]
    pub expect_no_errors: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaseKind {
    /// Go-to-definition on `symbol` in the first line containing `line_contains` must return a location whose URI
    /// contains `expect`.
    Definition {
        line_contains: String,
        symbol: String,
        expect: String,
    },
    /// The file must reach a state with no error diagnostics.
    NoErrors,
}

impl Case {
    pub fn kind(&self) -> anyhow::Result<CaseKind> {
        if self.expect_no_errors {
            return Ok(CaseKind::NoErrors);
        }
        match (&self.line_contains, &self.symbol, &self.expect) {
            (Some(l), Some(s), Some(e)) => Ok(CaseKind::Definition {
                line_contains: l.clone(),
                symbol: s.clone(),
                expect: e.clone(),
            }),
            _ => bail!(
                "case {:?} needs line_contains, symbol and expect, or expect_no_errors",
                self.name
            ),
        }
    }
}

/// 0-based (line, character) one past the start of the last `symbol` on the first line containing
/// `line_contains`, as the Python probe computed it.
pub fn position_of(text: &str, line_contains: &str, symbol: &str) -> Option<(u32, u32)> {
    text.lines()
        .enumerate()
        .find(|(_, l)| l.contains(line_contains))
        .and_then(|(n, line)| {
            let start = line.rfind(symbol)?;
            let character = line[..start].encode_utf16().count() + 1;
            Some((n as u32, character as u32))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_is_one_past_start_of_last_occurrence() {
        let text = "package a\n\nval x = Foo(Foo())\n";
        assert_eq!(position_of(text, "val x", "Foo"), Some((2, 13)));
        assert_eq!(position_of(text, "missing", "Foo"), None);
        assert_eq!(position_of(text, "val x", "Bar"), None);
    }

    #[test]
    fn parses_both_case_kinds() {
        let cases: Vec<Case> = serde_json::from_str(
            r#"[{"name":"d","file":"A.kt","line_contains":"import x","symbol":"X","expect":"x/X.kt"},
                {"name":"n","file":"A.kt","expect_no_errors":true}]"#,
        )
        .unwrap();
        assert_eq!(
            cases[0].kind().unwrap(),
            CaseKind::Definition {
                line_contains: "import x".into(),
                symbol: "X".into(),
                expect: "x/X.kt".into()
            }
        );
        assert_eq!(cases[1].kind().unwrap(), CaseKind::NoErrors);
    }

    #[test]
    fn incomplete_definition_case_is_rejected() {
        let case: Case =
            serde_json::from_str(r#"{"name":"d","file":"A.kt","symbol":"X"}"#).unwrap();
        assert!(case.kind().is_err());
    }
}
