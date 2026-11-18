# bazel-kotlin-lsp

Cross-module Kotlin navigation for Bazel (Bzlmod) repositories in Zed and other LSP editors. It runs a Bazel
aspect over your targets and writes the `workspace.json` project model that JetBrains' kotlin-lsp imports, so
go-to-definition, completion and find-usages work across Bazel modules and into library sources.

**Status: early, used daily on macOS.**

## Install (Zed, macOS or Linux)

You need Bazel 8 or later (`bazelisk` or `bazel` on your `PATH`) and Zed's **Kotlin** extension (Zed: Extensions,
search "Kotlin"), which gives Zed the Kotlin language.

### With the Zed extension

The `extension` folder is a Zed extension, "Bazel Kotlin". On first use it downloads this project's release for your
platform and kotlin-lsp 271.2085.0, then runs them for every Kotlin file. Until it is listed in Zed's extension
registry, install it from a clone; Zed builds it with Rust, which must be installed through
[rustup](https://rustup.rs):

```sh
git clone https://github.com/blacroix/bazel-kotlin-lsp
```

In Zed, run `zed: install dev extension` and pick the clone's `extension` folder. Then make it the Kotlin server
(`zed: open settings`):

```json
{
  "languages": {
    "Kotlin": {
      "language_servers": ["bazel-kotlin-lsp", "!kotlin-lsp", "!kotlin-language-server", "..."]
    }
  }
}
```

Optional settings, all under `"lsp": {"bazel-kotlin-lsp": {...}}`: `"settings"` takes `bazel_path`, `scope_dirs`,
`targets` (the `sync` flags below) and `kotlin_lsp_path`; `"binary": {"path": ...}` runs your own build of the proxy.

### By hand

**1. The proxy.** Pick your platform's target (`aarch64-apple-darwin`, `x86_64-apple-darwin`,
`x86_64-unknown-linux-musl` or `aarch64-unknown-linux-musl`) and download the latest release:

```sh
version=0.1.0
target=aarch64-apple-darwin
mkdir -p ~/.local/bin
curl -fsSL "https://github.com/blacroix/bazel-kotlin-lsp/releases/download/v$version/bazel-kotlin-lsp-$version-$target.tar.gz" \
  | tar -xz -C ~/.local/bin
~/.local/bin/bazel-kotlin-lsp --version
```

Use `curl` as above rather than a browser: macOS blocks unsigned binaries downloaded by a browser (if you did, run
`xattr -d com.apple.quarantine ~/.local/bin/bazel-kotlin-lsp`). With a Rust 1.99 toolchain you can instead build it:
`cargo install --git https://github.com/blacroix/bazel-kotlin-lsp bazel-kotlin-lsp`.

**2. kotlin-lsp 271.2085.0**, the version this project is tested with:

```sh
v=271.2085.0
mkdir -p ~/.local/share/kotlin-lsp && cd ~/.local/share/kotlin-lsp
# macOS, Apple silicon (drop "-aarch64" on Intel):
curl -fsSL -o kotlin-lsp.zip "https://download.jetbrains.com/language-server/kotlin-server/$v/kotlin-server-$v-aarch64.sit"
ditto -x -k kotlin-lsp.zip . && rm kotlin-lsp.zip
# Linux x86_64 (on arm64, add "-aarch64" before ".tar.gz"):
# curl -fsSL "https://download.jetbrains.com/language-server/kotlin-server/$v/kotlin-server-$v.tar.gz" | tar -xz
ls "kotlin-server-$v/bin/intellij-server"
```

**3. Zed settings** (`zed: open settings`). Replace `HOME` with your home folder (`echo $HOME`); Zed does not expand `~`:

```json
{
  "lsp": {
    "kotlin-lsp": {
      "binary": {
        "path": "HOME/.local/bin/bazel-kotlin-lsp",
        "arguments": [
          "proxy",
          "--",
          "HOME/.local/share/kotlin-lsp/kotlin-server-271.2085.0/bin/intellij-server",
          "--stdio"
        ]
      }
    }
  },
  "languages": {
    "Kotlin": {
      "language_servers": ["kotlin-lsp", "!kotlin-language-server", "..."]
    }
  }
}
```

Open your Bazel repository in Zed and open a Kotlin file. The first sync and indexing take about a minute ("Bazel sync"
then indexing in the status bar); after that, go-to-definition works across modules and into library sources. If a
sync fails, a notification shows Bazel's error with an "Open log" button. Outside Bazel repositories the proxy passes
everything through, so this setting is safe for every project.

## Usage

### In Zed, through the proxy

`bazel-kotlin-lsp proxy -- <kotlin-lsp command>` runs kotlin-lsp behind a small LSP proxy. In a Bzlmod repository it
points kotlin-lsp at the generated model and syncs in the background: once when the server starts, then 2 s after
you stop editing build files (`BUILD`, `MODULE.bazel`, `*.bzl`, `.bazelrc`, `.bazelversion`, `.bazelignore`,
`.bazel-kotlin.toml`, Maven `*_install.json`). Editing Kotlin files never triggers a sync, and the editor never waits
for one. kotlin-lsp picks up the new model by itself, without a restart. The sync shows as "Bazel sync" progress; a
failed or incomplete sync shows a notification that stays until you close it, with Bazel's error, the command to
reproduce it and an "Open log" button. The same error is shown once until a sync succeeds or the error changes.
Outside a Bzlmod repository the proxy passes every byte through unchanged, so it is safe to configure globally.

Zed runs it through the settings in [Install](#install-zed-macos-or-linux).

The proxy finds the repository from the folder the editor starts it in (in Zed, the project folder). `sync`'s flags
(`--workspace`, `--scope-dir`, `--target`, `--bazel`, `--shared-output-base`) go before `--`. The proxy writes only
LSP messages to stdout; its own notes and kotlin-lsp's go to stderr (Zed's language server log), and Bazel's output
goes to `sync.log` in the cache folder.

### On the command line

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

## Known limitations

- Code generated by KSP or Java annotation processors (Dagger, Moshi, Room, ...) is not modelled: those symbols do
  not resolve and show as errors. Sources generated by rules listed in `srcs` (such as a `genrule`) do work.
- Android targets (`kt_android_library` and similar) are skipped.
- Kotlin files only: Java files inside a module are indexed and reachable from Kotlin, but opening a Java file does
  not start kotlin-lsp.
- macOS and Linux only.

## Cost

`sync` runs Bazel with its own output base under the cache folder (`~/Library/Caches/bazel-kotlin-lsp` on macOS,
`~/.cache/bazel-kotlin-lsp` on Linux), so it never invalidates the analysis cache of your own builds. Downloads are
shared through Bazel's repository cache. Expect a few hundred MB of disk per repository (about 600 MB for a
28-module service with 240 Maven libraries) and one extra Bazel server (about 500 MB of RAM). Like any Bazel server,
it stays up for up to 3 hours after the last sync; to stop it sooner, run
`bazel --output_base=<printed folder>/../output_base shutdown` in your repository. Pass `--shared-output-base` to use
your own output base instead.

Each repository (each worktree, too) gets its own folder in the cache, holding the Bazel output base, the model and,
through the proxy, kotlin-lsp's index (a few hundred MB more). Opening a new worktree therefore costs one cold sync
and one full indexing, about a minute for a mid-sized service; reopening it is quick.

## Cleaning up

A cache folder records which workspace it belongs to. When the proxy starts, it removes the folders of deleted
workspaces, stopping their Bazel server first: a deleted worktree's cache goes the next time any Kotlin project opens.
A workspace whose parent folder is missing too (an unmounted drive) is only considered gone after 3 days unused. To do
it by hand:

```sh
bazel-kotlin-lsp gc --dry-run              # list what would be removed
bazel-kotlin-lsp gc                        # remove it
bazel-kotlin-lsp gc --workspace /path/to/old-worktree   # remove one workspace's cache now
```

Folders written by versions without that record are removed once untouched for 14 days. Indexes that kotlin-lsp
built in its own cache before the proxy chose the location
(`~/Library/Caches/JetBrains/IntelliJServer/workspaces` on macOS) are not touched; delete them by hand if needed.
