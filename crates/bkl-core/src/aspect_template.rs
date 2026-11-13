//! The Starlark aspect, embedded and rendered with the workspace's canonical repository names.

pub const ASPECT_REPO: &str = "bazel_kotlin_lsp";
pub const ASPECT: &str = "@bazel_kotlin_lsp//:aspect.bzl%bkl_aspect";
pub const JSON_GROUP: &str = "bkl_json";
pub const JARS_GROUP: &str = "bkl_jars";

const TEMPLATE: &str = include_str!("../../../aspect/aspect.bzl.tpl");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleRepos {
    pub rules_java: String,
    pub rules_kotlin: String,
}

/// Files of the injected repository, as (file name, contents).
pub fn render_aspect(repos: &RuleRepos) -> Vec<(&'static str, String)> {
    let bzl = TEMPLATE
        .replace("%{rules_java}", &repos.rules_java)
        .replace("%{rules_kotlin}", &repos.rules_kotlin);
    vec![
        ("aspect.bzl", bzl),
        ("BUILD.bazel", String::new()),
        ("REPO.bazel", String::new()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_substitutes_canonical_names() {
        let files = render_aspect(&RuleRepos {
            rules_java: "rules_java+".into(),
            rules_kotlin: "rules_kotlin+".into(),
        });
        let bzl = &files.iter().find(|(n, _)| *n == "aspect.bzl").unwrap().1;
        assert!(bzl.contains("@@rules_java+//java/common:java_info.bzl"));
        assert!(!bzl.contains("%{"));
    }
}
