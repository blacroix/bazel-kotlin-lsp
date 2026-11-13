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
