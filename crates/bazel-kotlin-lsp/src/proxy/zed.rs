//! Whether Zed runs another Kotlin language server next to this one, from its settings files.
//!
//! An extension cannot change Zed's settings, and Zed's default enables the Kotlin extension's kotlin-lsp too, so
//! the proxy reads the settings and tells the user what to add.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Zed's built-in `languages.Kotlin.language_servers`.
const DEFAULT_KOTLIN: [&str; 3] = ["!kotlin-language-server", "kotlin-lsp", "..."];
/// The Kotlin extension's servers.
const OTHER_KOTLIN: [&str; 2] = ["kotlin-lsp", "kotlin-language-server"];

#[derive(Debug, PartialEq, Eq)]
pub struct Duplicate {
    /// The other Kotlin servers Zed runs.
    pub others: Vec<String>,
    /// The settings file that chooses the Kotlin servers here.
    pub settings: PathBuf,
    pub in_project: bool,
}

/// Zed's user `settings.json`.
pub fn user_settings_file(env: &dyn Fn(&str) -> Option<String>, macos: bool) -> Option<PathBuf> {
    let absolute = |name: &str| env(name).map(PathBuf::from).filter(|p| p.is_absolute());
    let home_config = || absolute("HOME").map(|home| home.join(".config"));
    let config = if macos {
        home_config()?
    } else {
        absolute("FLATPAK_XDG_CONFIG_HOME")
            .or_else(|| absolute("XDG_CONFIG_HOME"))
            .or_else(home_config)?
    };
    Some(config.join("zed").join("settings.json"))
}

/// The project's `.zed/settings.json` files from `start` up to `root`, nearest first.
pub fn project_settings_files(start: &Path, root: &Path) -> Vec<PathBuf> {
    start
        .ancestors()
        .take_while(|dir| dir.starts_with(root))
        .map(|dir| dir.join(".zed").join("settings.json"))
        .filter(|file| file.is_file())
        .collect()
}

/// The other Kotlin servers Zed runs next to `own`, if any; `None` too when a settings file cannot be read.
pub fn duplicate(own: &str, user: Option<&Path>, project: &[PathBuf]) -> Option<Duplicate> {
    for file in project {
        if let Some(servers) = kotlin_servers(file)? {
            return duplicate_in(own, &servers, file, true);
        }
    }
    let servers = match user {
        Some(file) if file.exists() => kotlin_servers(file)?,
        _ => None,
    };
    let servers = servers.unwrap_or_else(|| DEFAULT_KOTLIN.iter().map(|s| s.to_string()).collect());
    duplicate_in(own, &servers, user.unwrap_or(Path::new("")), false)
}

/// The notice's Markdown text.
pub fn notice_text(own: &str, duplicate: &Duplicate) -> String {
    let others = duplicate
        .others
        .iter()
        .map(|s| format!("**{s}**"))
        .collect::<Vec<_>>()
        .join(" and ");
    let mut servers = vec![format!("\"{own}\"")];
    servers.extend(
        OTHER_KOTLIN
            .iter()
            .filter(|s| **s != own)
            .map(|s| format!("\"!{s}\"")),
    );
    servers.push("\"...\"".into());
    let setting = format!(
        "\"languages\": {{ \"Kotlin\": {{ \"language_servers\": [{}] }} }}",
        servers.join(", ")
    );
    let place = if duplicate.in_project {
        format!(
            "`{}`, which chooses the Kotlin servers for this project",
            duplicate.settings.display()
        )
    } else {
        "Zed's settings (`zed: open settings`)".to_string()
    };
    format!(
        "Zed also runs {others} from the Kotlin extension here, so each Kotlin file gets a second server that does \
         not see the Bazel targets. To keep only {own}, add this to {place}:\n\n```json\n{setting}\n```"
    )
}

/// Parses Zed's JSON with comments and trailing commas.
pub fn parse_jsonc(text: &str) -> Option<Value> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                out.push(c);
                while let Some(c) = chars.next() {
                    out.push(c);
                    match c {
                        '\\' => out.extend(chars.next()),
                        '"' => break,
                        _ => {}
                    }
                }
            }
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push(c);
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut last = ' ';
                for c in chars.by_ref() {
                    if last == '*' && c == '/' {
                        break;
                    }
                    last = c;
                }
                out.push(' ');
            }
            '}' | ']' => {
                let kept = out.trim_end().len();
                out.truncate(kept);
                if out.ends_with(',') {
                    out.pop();
                }
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    serde_json::from_str(&out).ok()
}

/// `languages.Kotlin.language_servers` in `file`: `None` if unreadable, `Some(None)` if unset.
fn kotlin_servers(file: &Path) -> Option<Option<Vec<String>>> {
    let settings = parse_jsonc(&std::fs::read_to_string(file).ok()?)?;
    let Some(servers) = settings.pointer("/languages/Kotlin/language_servers") else {
        return Some(None);
    };
    Some(Some(
        servers
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
    ))
}

fn duplicate_in(
    own: &str,
    servers: &[String],
    settings: &Path,
    in_project: bool,
) -> Option<Duplicate> {
    let enabled = |name: &str| {
        servers.iter().any(|s| s == name)
            || (servers.iter().any(|s| s == "...")
                && !servers.iter().any(|s| s.strip_prefix('!') == Some(name)))
    };
    if !enabled(own) {
        return None;
    }
    let others: Vec<String> = OTHER_KOTLIN
        .iter()
        .filter(|s| **s != own && enabled(s))
        .map(|s| s.to_string())
        .collect();
    (!others.is_empty()).then(|| Duplicate {
        others,
        settings: settings.to_path_buf(),
        in_project,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const OWN: &str = "bazel-kotlin-lsp";

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            vars.iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn user_settings_live_under_the_config_folder() {
        assert_eq!(
            user_settings_file(&env(&[("HOME", "/h"), ("XDG_CONFIG_HOME", "/x")]), true),
            Some("/h/.config/zed/settings.json".into())
        );
        assert_eq!(
            user_settings_file(&env(&[("HOME", "/h"), ("XDG_CONFIG_HOME", "/x")]), false),
            Some("/x/zed/settings.json".into())
        );
        assert_eq!(
            user_settings_file(&env(&[("HOME", "/h"), ("XDG_CONFIG_HOME", "rel")]), false),
            Some("/h/.config/zed/settings.json".into())
        );
        assert_eq!(
            user_settings_file(
                &env(&[
                    ("HOME", "/h"),
                    ("XDG_CONFIG_HOME", "/x"),
                    ("FLATPAK_XDG_CONFIG_HOME", "/f")
                ]),
                false
            ),
            Some("/f/zed/settings.json".into())
        );
        assert_eq!(user_settings_file(&env(&[("HOME", "rel")]), true), None);
        assert_eq!(user_settings_file(&env(&[]), false), None);
    }

    #[test]
    fn jsonc_comments_and_trailing_commas_are_read() {
        let text = r#"// Zed settings
        {
          /* block
             comment */
          "url": "https://example.org/a//b", // trailing
          "quote": "say \"hi\" // not a comment",
          "list": ["a", "b",],
        }"#;
        assert_eq!(
            parse_jsonc(text),
            Some(
                json!({"url": "https://example.org/a//b", "quote": "say \"hi\" // not a comment", "list": ["a", "b"]})
            )
        );
        assert_eq!(parse_jsonc("{ \"a\": , }"), None);
    }

    fn settings(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    fn kotlin(servers: &str) -> String {
        format!("{{\"languages\": {{\"Kotlin\": {{\"language_servers\": {servers}}}}}}}")
    }

    #[test]
    fn zed_defaults_run_kotlin_lsp_too() {
        let t = tempfile::tempdir().unwrap();
        let user = settings(t.path(), "settings.json", "{\"theme\": \"One Dark\"}");
        assert_eq!(
            duplicate(OWN, Some(&user), &[]),
            Some(Duplicate {
                others: vec!["kotlin-lsp".into()],
                settings: user,
                in_project: false
            })
        );
    }

    #[test]
    fn the_recommended_setting_runs_only_this_server() {
        let t = tempfile::tempdir().unwrap();
        let user = settings(
            t.path(),
            "settings.json",
            &kotlin(r#"["bazel-kotlin-lsp", "!kotlin-lsp", "!kotlin-language-server", "..."]"#),
        );
        assert_eq!(duplicate(OWN, Some(&user), &[]), None);
        let only = settings(t.path(), "only.json", &kotlin(r#"["bazel-kotlin-lsp"]"#));
        assert_eq!(duplicate(OWN, Some(&only), &[]), None);
    }

    #[test]
    fn a_wildcard_without_exclusions_runs_both_kotlin_extension_servers() {
        let t = tempfile::tempdir().unwrap();
        let user = settings(
            t.path(),
            "settings.json",
            &kotlin(r#"["bazel-kotlin-lsp", "..."]"#),
        );
        assert_eq!(
            duplicate(OWN, Some(&user), &[]).unwrap().others,
            ["kotlin-lsp", "kotlin-language-server"]
        );
    }

    #[test]
    fn the_nearest_project_setting_wins() {
        let t = tempfile::tempdir().unwrap();
        let user = settings(
            t.path(),
            "settings.json",
            &kotlin(r#"["bazel-kotlin-lsp", "!kotlin-lsp", "!kotlin-language-server", "..."]"#),
        );
        let near = settings(t.path(), "near/.zed/settings.json", &kotlin(r#"["..."]"#));
        let far = settings(
            t.path(),
            "far/.zed/settings.json",
            &kotlin(r#"["bazel-kotlin-lsp"]"#),
        );
        assert_eq!(
            duplicate(OWN, Some(&user), &[near.clone(), far.clone()]),
            Some(Duplicate {
                others: vec!["kotlin-lsp".into(), "kotlin-language-server".into()],
                settings: near,
                in_project: true
            })
        );
        let silent = settings(t.path(), "silent/.zed/settings.json", "{\"tab_size\": 2}");
        assert_eq!(duplicate(OWN, Some(&user), &[silent, far]), None);
    }

    #[test]
    fn unreadable_settings_raise_no_notice() {
        let t = tempfile::tempdir().unwrap();
        let broken = settings(t.path(), "settings.json", "{ \"languages\": ");
        assert_eq!(duplicate(OWN, Some(&broken), &[]), None);
        assert_eq!(
            duplicate(OWN, Some(&t.path().join("missing.json")), &[])
                .unwrap()
                .others,
            ["kotlin-lsp"],
            "a missing file means Zed's defaults"
        );
    }

    #[test]
    fn a_proxy_running_as_kotlin_lsp_is_not_its_own_duplicate() {
        assert_eq!(duplicate("kotlin-lsp", None, &[]), None);
    }

    #[test]
    fn project_settings_files_are_found_from_the_start_folder_up_to_the_root() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("repo");
        let start = root.join("services/a");
        std::fs::create_dir_all(&start).unwrap();
        let near = settings(&start, ".zed/settings.json", "{}");
        let top = settings(&root, ".zed/settings.json", "{}");
        settings(t.path(), ".zed/settings.json", "{}");
        assert_eq!(project_settings_files(&start, &root), [near, top]);
    }

    #[test]
    fn the_notice_names_the_other_servers_and_the_setting_to_add() {
        let user = Duplicate {
            others: vec!["kotlin-lsp".into()],
            settings: "/h/.config/zed/settings.json".into(),
            in_project: false,
        };
        let text = notice_text(OWN, &user);
        for expected in [
            "kotlin-lsp",
            r#""language_servers": ["bazel-kotlin-lsp", "!kotlin-lsp", "!kotlin-language-server", "..."]"#,
            "zed: open settings",
        ] {
            assert!(text.contains(expected), "{expected:?} missing from {text}");
        }
        let project = Duplicate {
            in_project: true,
            settings: "/repo/.zed/settings.json".into(),
            ..user
        };
        assert!(notice_text(OWN, &project).contains("/repo/.zed/settings.json"));
    }
}
