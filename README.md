# bazel-kotlin-lsp

Cross-module Kotlin navigation for Bazel (Bzlmod) repositories in Zed and other LSP editors. It runs a Bazel
aspect over your targets and writes the `workspace.json` project model that JetBrains' kotlin-lsp imports, so
go-to-definition, completion and find-usages work across Bazel modules and into library sources.

**Status: work in progress.**

## Usage

```sh
bazel-kotlin-lsp sync --workspace /path/to/repo
```

`sync` prints the folder holding the generated `workspace.json`, and nothing else: logs, build events and Bazel's
output base sit next to it. Point kotlin-lsp at that folder with the initialization option
`{"projects": [{"type": "json", "path": "file:///that/folder"}]}`. Nothing is written into your repository.

## Supported versions

- Bazel 8.0.0 or later, with Bzlmod (`MODULE.bazel`). WORKSPACE-only repositories are not supported.
- Tested in CI on Bazel 8.0.0 (with rules_kotlin 2.1.0, rules_java 8.6.0, rules_jvm_external 6.6), 8.8.2 and 9.3.2
  (with rules_kotlin 2.4.21, rules_java 9.9.0, rules_jvm_external 7.2). Recent rule versions may themselves need a
  recent Bazel 8.x.
- kotlin-lsp 271.2085.0.

## Cost

`sync` runs Bazel with its own output base under the cache folder (`~/Library/Caches/bazel-kotlin-lsp` on macOS,
`~/.cache/bazel-kotlin-lsp` on Linux), so it never invalidates the analysis cache of your own builds. Downloads are
shared through Bazel's repository cache. Expect a few hundred MB of disk per repository (about 600 MB for a
28-module service with 240 Maven libraries) and one extra Bazel server (about 500 MB of RAM). Like any Bazel server,
it stays up for up to 3 hours after the last sync; to stop it sooner, run
`bazel --output_base=<printed folder>/../output_base shutdown` in your repository. Pass `--shared-output-base` to use
your own output base instead.
