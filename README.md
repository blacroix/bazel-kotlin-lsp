# bazel-kotlin-lsp

Cross-module Kotlin navigation for Bazel (Bzlmod) repositories in Zed and other LSP editors. It runs a Bazel
aspect over your targets and writes the `workspace.json` project model that JetBrains' kotlin-lsp imports, so
go-to-definition, completion and find-usages work across Bazel modules and into library sources.

**Status: work in progress.**

## Usage

```sh
bazel-kotlin-lsp sync --workspace /path/to/repo
```

`sync` prints the folder holding the generated `workspace.json`. Point kotlin-lsp at it with the initialization
option `{"projects": [{"type": "json", "path": "file:///that/folder"}]}`. Nothing is written into your repository.

## Cost

`sync` runs Bazel with its own output base under the cache folder (`~/Library/Caches/bazel-kotlin-lsp` on macOS,
`~/.cache/bazel-kotlin-lsp` on Linux), so it never invalidates the analysis cache of your own builds. Downloads are
shared through Bazel's repository cache. Expect a few hundred MB of disk per repository (about 600 MB for a
28-module service with 240 Maven libraries) and one extra Bazel server (about 500 MB of RAM while it runs). Pass
`--shared-output-base` to use your own output base instead.
