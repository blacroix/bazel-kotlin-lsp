use serde::Deserialize;
use zed_extension_api::lsp::{Completion, CompletionKind};
use zed_extension_api::settings::LspSettings;
use zed_extension_api::{
    self as zed, Architecture, DownloadedFileType, LanguageServerId,
    LanguageServerInstallationStatus, Os, Worktree,
};

const REPOSITORY: &str = "blacroix/bazel-kotlin-lsp";
const KOTLIN_LSP_VERSION: &str = "271.2085.0";

/// `lsp.bazel-kotlin-lsp.settings` in Zed's settings.
#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
struct Options {
    kotlin_lsp_path: Option<String>,
    bazel_path: Option<String>,
    scope_dirs: Vec<String>,
    targets: Vec<String>,
}

/// The Rust target of the proxy's release archive for this platform.
fn target_triple(os: Os, arch: Architecture) -> Result<&'static str, String> {
    match (os, arch) {
        (Os::Mac, Architecture::Aarch64) => Ok("aarch64-apple-darwin"),
        (Os::Mac, Architecture::X8664) => Ok("x86_64-apple-darwin"),
        (Os::Linux, Architecture::X8664) => Ok("x86_64-unknown-linux-musl"),
        (Os::Linux, Architecture::Aarch64) => Ok("aarch64-unknown-linux-musl"),
        _ => Err("bazel-kotlin-lsp supports macOS and Linux on x86_64 and arm64".into()),
    }
}

fn proxy_url(version: &str, triple: &str) -> String {
    format!(
        "https://github.com/{REPOSITORY}/releases/download/v{version}/bazel-kotlin-lsp-{version}-{triple}.tar.gz"
    )
}

/// The kotlin-lsp archive for this platform: its URL and how to unpack it.
fn kotlin_lsp_archive(os: Os, arch: Architecture) -> Result<(String, DownloadedFileType), String> {
    let arch = match arch {
        Architecture::Aarch64 => "-aarch64",
        Architecture::X8664 => "",
        Architecture::X86 => return Err("kotlin-lsp does not support 32-bit x86".into()),
    };
    // JetBrains' macOS `.sit` archive is a zip.
    let (extension, kind) = match os {
        Os::Mac => ("sit", DownloadedFileType::Zip),
        Os::Linux => ("tar.gz", DownloadedFileType::GzipTar),
        Os::Windows => return Err("bazel-kotlin-lsp does not support Windows".into()),
    };
    let v = KOTLIN_LSP_VERSION;
    Ok((
        format!(
            "https://download.jetbrains.com/language-server/kotlin-server/{v}/kotlin-server-{v}{arch}.{extension}"
        ),
        kind,
    ))
}

/// The proxy's arguments: its flags, then the kotlin-lsp command after `--`.
fn proxy_args(server: &str, options: &Options, kotlin_lsp: &str) -> Vec<String> {
    let mut args = vec!["proxy".to_string(), "--zed-server".into(), server.into()];
    if let Some(bazel) = &options.bazel_path {
        args.extend(["--bazel".to_string(), bazel.clone()]);
    }
    for dir in &options.scope_dirs {
        args.extend(["--scope-dir".to_string(), dir.clone()]);
    }
    for target in &options.targets {
        args.extend(["--target".to_string(), target.clone()]);
    }
    args.extend(["--".into(), kotlin_lsp.into(), "--stdio".into()]);
    args
}

/// Downloads an archive into `dir` unless `binary` (inside it) exists, removing other versions of the same
/// download; returns the binary's absolute path, since the proxy starts kotlin-lsp from the project folder.
fn install(
    id: &LanguageServerId,
    url: &str,
    kind: DownloadedFileType,
    prefix: &str,
    dir: &str,
    binary: &str,
) -> zed::Result<String> {
    if !std::fs::metadata(binary).is_ok_and(|m| m.is_file()) {
        zed::set_language_server_installation_status(
            id,
            &LanguageServerInstallationStatus::Downloading,
        );
        zed::download_file(url, dir, kind).map_err(|e| format!("downloading {url}: {e}"))?;
        zed::make_file_executable(binary)?;
        for entry in std::fs::read_dir(".").into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(prefix) && name != dir {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    let cwd = std::env::current_dir().map_err(|e| format!("extension folder: {e}"))?;
    Ok(cwd.join(binary).to_string_lossy().into_owned())
}

struct BazelKotlin {
    proxy: Option<String>,
    kotlin_lsp: Option<String>,
}

impl BazelKotlin {
    fn proxy_path(&mut self, id: &LanguageServerId) -> zed::Result<String> {
        if let Some(path) = &self.proxy {
            return Ok(path.clone());
        }
        let (os, arch) = zed::current_platform();
        let version = env!("CARGO_PKG_VERSION");
        let dir = format!("bazel-kotlin-lsp-{version}");
        let path = install(
            id,
            &proxy_url(version, target_triple(os, arch)?),
            DownloadedFileType::GzipTar,
            "bazel-kotlin-lsp-",
            &dir,
            &format!("{dir}/bazel-kotlin-lsp"),
        )?;
        self.proxy = Some(path.clone());
        Ok(path)
    }

    fn kotlin_lsp_path(&mut self, id: &LanguageServerId) -> zed::Result<String> {
        if let Some(path) = &self.kotlin_lsp {
            return Ok(path.clone());
        }
        let (os, arch) = zed::current_platform();
        let (url, kind) = kotlin_lsp_archive(os, arch)?;
        let v = KOTLIN_LSP_VERSION;
        let dir = format!("kotlin-lsp-{v}");
        let path = install(
            id,
            &url,
            kind,
            "kotlin-lsp-",
            &dir,
            &format!("{dir}/kotlin-server-{v}/bin/intellij-server"),
        )?;
        self.kotlin_lsp = Some(path.clone());
        Ok(path)
    }
}

impl zed::Extension for BazelKotlin {
    fn new() -> Self {
        BazelKotlin {
            proxy: None,
            kotlin_lsp: None,
        }
    }

    fn language_server_command(
        &mut self,
        id: &LanguageServerId,
        worktree: &Worktree,
    ) -> zed::Result<zed::Command> {
        let settings = LspSettings::for_worktree(id.as_ref(), worktree).unwrap_or_default();
        let options: Options = match settings.settings {
            Some(value) => {
                zed::serde_json::from_value(value).map_err(|e| format!("lsp.{id}.settings: {e}"))?
            }
            None => Options::default(),
        };
        let proxy = match settings.binary.and_then(|b| b.path) {
            Some(path) => path,
            None => self.proxy_path(id)?,
        };
        let kotlin_lsp = match &options.kotlin_lsp_path {
            Some(path) => path.clone(),
            None => self.kotlin_lsp_path(id)?,
        };
        zed::set_language_server_installation_status(id, &LanguageServerInstallationStatus::None);
        Ok(zed::Command {
            command: proxy,
            args: proxy_args(id.as_ref(), &options, &kotlin_lsp),
            env: worktree.shell_env(),
        })
    }

    fn label_for_completion(
        &self,
        _id: &LanguageServerId,
        completion: Completion,
    ) -> Option<zed::CodeLabel> {
        let details = completion.label_details.as_ref();
        completion_label(
            &completion.label,
            completion.kind.as_ref(),
            details.and_then(|d| d.detail.as_deref()),
            details.and_then(|d| d.description.as_deref()),
        )
    }
}

/// The completion menu label: Zed's own leaves out `labelDetails.detail`, where kotlin-lsp puts the package.
fn completion_label(
    label: &str,
    kind: Option<&CompletionKind>,
    detail: Option<&str>,
    description: Option<&str>,
) -> Option<zed::CodeLabel> {
    let detail = detail.filter(|d| !d.is_empty());
    let description = description.filter(|d| !d.is_empty());
    if detail.is_none() && description.is_none() {
        return None;
    }
    let dim = || Some("comment".to_string());
    let mut spans = vec![zed::CodeLabelSpan::literal(
        label,
        kind.and_then(highlight).map(String::from),
    )];
    spans.extend(detail.map(|d| zed::CodeLabelSpan::literal(shorten_detail(d), dim())));
    spans.extend(description.map(|d| zed::CodeLabelSpan::literal(format!(" {d}"), dim())));
    Some(zed::CodeLabel {
        code: String::new(),
        spans,
        filter_range: (0..label.len()).into(),
    })
}

/// `detail` with its package shortened: Zed cuts the completion menu at a fixed width, losing a long package's end.
fn shorten_detail(detail: &str) -> String {
    let in_parentheses = detail
        .strip_suffix(')')
        .and_then(|d| d.rsplit_once(" ("))
        .map(|(head, package)| (format!("{head} ("), package, ")"));
    let after_in = || {
        detail
            .rsplit_once(" in ")
            .map(|(head, package)| (format!("{head} in "), package, ""))
    };
    match in_parentheses
        .filter(|(_, p, _)| is_package(p))
        .or_else(|| after_in().filter(|(_, p, _)| is_package(p)))
    {
        Some((head, package, tail)) => format!("{head}{}{tail}", shorten_package(package)),
        None => detail.to_string(),
    }
}

fn is_package(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
}

/// Abbreviates segments to their initial, left to right and never the last, until the package fits.
fn shorten_package(package: &str) -> String {
    const FITS: usize = 30;
    let mut segments: Vec<String> = package.split('.').map(String::from).collect();
    let length = |segments: &[String]| segments.iter().map(|s| s.len() + 1).sum::<usize>() - 1;
    for i in 0..segments.len().saturating_sub(1) {
        if length(&segments) <= FITS {
            break;
        }
        if let Some(initial) = segments[i].chars().next() {
            segments[i] = initial.to_string();
        }
    }
    segments.join(".")
}

/// The highlight Zed's own label gives a completion of this kind.
fn highlight(kind: &CompletionKind) -> Option<&'static str> {
    match kind {
        CompletionKind::Class
        | CompletionKind::Interface
        | CompletionKind::Struct
        | CompletionKind::Enum => Some("type"),
        CompletionKind::Constant => Some("constant"),
        CompletionKind::Constructor => Some("constructor"),
        CompletionKind::EnumMember | CompletionKind::Field | CompletionKind::Property => {
            Some("property")
        }
        CompletionKind::Function | CompletionKind::Method => Some("function"),
        CompletionKind::Variable => Some("variable"),
        CompletionKind::Keyword => Some("keyword"),
        _ => None,
    }
}

zed::register_extension!(BazelKotlin);

#[cfg(test)]
mod tests {
    use super::*;

    fn text(label: &zed::CodeLabel) -> String {
        label
            .spans
            .iter()
            .map(|span| match span {
                zed::CodeLabelSpan::Literal(literal) => literal.text.as_str(),
                zed::CodeLabelSpan::CodeRange(_) => panic!("labels are literals"),
            })
            .collect()
    }

    fn highlights(label: &zed::CodeLabel) -> Vec<Option<&str>> {
        label
            .spans
            .iter()
            .map(|span| match span {
                zed::CodeLabelSpan::Literal(literal) => literal.highlight_name.as_deref(),
                zed::CodeLabelSpan::CodeRange(_) => panic!("labels are literals"),
            })
            .collect()
    }

    #[test]
    fn a_class_label_shows_its_package() {
        let label = completion_label(
            "UserId",
            Some(&CompletionKind::Class),
            Some(" (com.example.session)"),
            None,
        )
        .unwrap();
        assert_eq!(text(&label), "UserId (com.example.session)");
        assert_eq!(highlights(&label), [Some("type"), Some("comment")]);
        assert_eq!((label.filter_range.start, label.filter_range.end), (0, 6));
    }

    #[test]
    fn a_function_label_shows_its_parameters_package_and_type() {
        let label = completion_label(
            "describe",
            Some(&CompletionKind::Function),
            Some("(shape: Shape) (fixture.app)"),
            Some("Report"),
        )
        .unwrap();
        assert_eq!(text(&label), "describe(shape: Shape) (fixture.app) Report");
        assert_eq!(
            highlights(&label),
            [Some("function"), Some("comment"), Some("comment")]
        );
    }

    #[test]
    fn long_packages_shrink_from_the_left_until_they_fit() {
        assert_eq!(
            shorten_package("com.example.shop.game.server.games.session"),
            "c.e.s.g.server.games.session"
        );
        assert_eq!(
            shorten_package("com.example.shop.game.server.infrastructure.driving.http.dto"),
            "c.e.s.g.s.i.driving.http.dto"
        );
        assert_eq!(
            shorten_package("io.ktor.server.auth"),
            "io.ktor.server.auth"
        );
        assert_eq!(shorten_package("a.b"), "a.b");
    }

    #[test]
    fn the_package_in_a_detail_is_shortened_and_the_rest_kept() {
        let long = "com.example.shop.game.server.games.session";
        assert_eq!(
            shorten_detail(&format!(" ({long})")),
            " (c.e.s.g.server.games.session)"
        );
        assert_eq!(
            shorten_detail(&format!("(shape: Shape) ({long})")),
            "(shape: Shape) (c.e.s.g.server.games.session)"
        );
        assert_eq!(
            shorten_detail(&format!(
                "(transform: (Int) -> R) for Iterable<T> in {long}"
            )),
            "(transform: (Int) -> R) for Iterable<T> in c.e.s.g.server.games.session"
        );
        assert_eq!(shorten_detail("(shape: Shape)"), "(shape: Shape)");
        assert_eq!(shorten_detail(" { f: (Int) -> R }"), " { f: (Int) -> R }");
    }

    #[test]
    fn without_label_details_zed_keeps_its_own_label() {
        assert!(completion_label("x", Some(&CompletionKind::Variable), None, None).is_none());
        assert!(completion_label("x", None, Some(""), Some("")).is_none());
    }

    #[test]
    fn every_supported_platform_has_a_release_target() {
        assert_eq!(
            target_triple(Os::Mac, Architecture::Aarch64),
            Ok("aarch64-apple-darwin")
        );
        assert_eq!(
            target_triple(Os::Mac, Architecture::X8664),
            Ok("x86_64-apple-darwin")
        );
        assert_eq!(
            target_triple(Os::Linux, Architecture::X8664),
            Ok("x86_64-unknown-linux-musl")
        );
        assert_eq!(
            target_triple(Os::Linux, Architecture::Aarch64),
            Ok("aarch64-unknown-linux-musl")
        );
        assert!(target_triple(Os::Windows, Architecture::X8664).is_err());
    }

    #[test]
    fn proxy_url_names_the_release_asset() {
        assert_eq!(
            proxy_url("0.1.0", "aarch64-apple-darwin"),
            "https://github.com/blacroix/bazel-kotlin-lsp/releases/download/v0.1.0/bazel-kotlin-lsp-0.1.0-aarch64-apple-darwin.tar.gz"
        );
    }

    #[test]
    fn kotlin_lsp_archive_per_platform() {
        let base = "https://download.jetbrains.com/language-server/kotlin-server/271.2085.0";
        let (url, kind) = kotlin_lsp_archive(Os::Mac, Architecture::Aarch64).unwrap();
        assert_eq!(url, format!("{base}/kotlin-server-271.2085.0-aarch64.sit"));
        assert!(matches!(kind, DownloadedFileType::Zip));
        let (url, kind) = kotlin_lsp_archive(Os::Linux, Architecture::X8664).unwrap();
        assert_eq!(url, format!("{base}/kotlin-server-271.2085.0.tar.gz"));
        assert!(matches!(kind, DownloadedFileType::GzipTar));
        assert!(kotlin_lsp_archive(Os::Windows, Architecture::X8664).is_err());
    }

    #[test]
    fn proxy_args_carry_the_settings_then_kotlin_lsp() {
        assert_eq!(
            proxy_args(
                "bazel-kotlin-lsp",
                &Options::default(),
                "/k/intellij-server"
            ),
            [
                "proxy",
                "--zed-server",
                "bazel-kotlin-lsp",
                "--",
                "/k/intellij-server",
                "--stdio"
            ]
        );
        let options = Options {
            bazel_path: Some("/opt/bazel".into()),
            scope_dirs: vec!["app".into(), "-app/legacy".into()],
            targets: vec!["//tools:gen".into()],
            ..Default::default()
        };
        assert_eq!(
            proxy_args("bazel-kotlin-lsp", &options, "/k/intellij-server"),
            [
                "proxy",
                "--zed-server",
                "bazel-kotlin-lsp",
                "--bazel",
                "/opt/bazel",
                "--scope-dir",
                "app",
                "--scope-dir",
                "-app/legacy",
                "--target",
                "//tools:gen",
                "--",
                "/k/intellij-server",
                "--stdio"
            ]
        );
    }

    #[test]
    fn options_parse_from_zed_settings() {
        let options: Options = zed::serde_json::from_value(zed::serde_json::json!({
            "kotlin_lsp_path": "/k",
            "scope_dirs": ["app"]
        }))
        .unwrap();
        assert_eq!(options.kotlin_lsp_path.as_deref(), Some("/k"));
        assert_eq!(options.scope_dirs, ["app"]);
        assert!(options.targets.is_empty());
    }
}
