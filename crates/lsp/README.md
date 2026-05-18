# rpm-spec-lsp

[![CI](https://github.com/johnlepikhin/rpm-spec-tool/actions/workflows/ci.yml/badge.svg)](https://github.com/johnlepikhin/rpm-spec-tool/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

Language Server Protocol (LSP) implementation for RPM `.spec` files,
built on top of the [`rpm-spec-analyzer`](../analyzer) library. Surfaces
all 100+ analyzer lints as inline diagnostics, exposes their machine-
applicable fixes as quick actions, and provides outline / hover /
completion based on a parsed AST. Sync stdio server in the style of
`rust-analyzer` — no Tokio, one binary, works with every mainstream
editor that speaks LSP.

## Features

| Capability                                                  | Status |
|-------------------------------------------------------------|:------:|
| `textDocument/publishDiagnostics` (analyzer lints + URL)    |   ✓    |
| `textDocument/codeAction` (quick fixes)                     |   ✓    |
| `textDocument/documentSymbol` (outline)                     |   ✓    |
| `textDocument/foldingRange` (sections + `%if` blocks)       |   ✓    |
| `textDocument/hover` (tags, directives, profile macros)     |   ✓    |
| `textDocument/completion` (tags, directives, profile macros)|   ✓    |
| `textDocument/inlayHint` (macro expansion from profile)     |   ✓    |
| `textDocument/definition` (macro → `%define` site)          |   ✓    |
| `textDocument/references` (macro usages)                    |   ✓    |
| `textDocument/documentHighlight` (same-name occurrences)    |   ✓    |
| `textDocument/prepareRename` + `rename` (macros)            |   ✓    |
| `workspace/didChangeWatchedFiles` (auto-reload config)      |   ✓    |
| `.rpmspec.toml` discovery (upward walk)                     |   ✓    |
| UTF-8 / UTF-16 position encoding negotiation                |   ✓    |
| `textDocument/formatting`                                   |   —    |
| Sub-package / Source / Patch rename                         |   —    |
| Semantic tokens (use the editor's tree-sitter grammar)      |   —    |

## Install

The binary is part of the `rpm-spec-tool` workspace. Build and install
it with:

```bash
cargo install --git https://github.com/johnlepikhin/rpm-spec-tool rpm-spec-lsp
```

Or, from a local checkout:

```bash
cargo build --release -p rpm-spec-lsp
# Binary lands in ./target/release/rpm-spec-lsp
```

## Editor setup

### Neovim (`nvim-lspconfig` ≥ 0.1.7)

```lua
vim.lsp.config('rpm_spec_lsp', {
  cmd = { 'rpm-spec-lsp' },
  filetypes = { 'spec' },
  root_dir = function(_, ctx)
    return vim.fs.root(ctx.bufnr, { '.rpmspec.toml', '.git' })
           or vim.fn.getcwd()
  end,
})
vim.lsp.enable('rpm_spec_lsp')
```

For older Neovim versions falling back to the classic
`require('lspconfig')` API:

```lua
local lspconfig = require('lspconfig')
local configs   = require('lspconfig.configs')
if not configs.rpm_spec_lsp then
  configs.rpm_spec_lsp = {
    default_config = {
      cmd = { 'rpm-spec-lsp' },
      filetypes = { 'spec' },
      root_dir = lspconfig.util.root_pattern('.rpmspec.toml', '.git'),
      single_file_support = true,
    },
  }
end
lspconfig.rpm_spec_lsp.setup({})
```

### Helix (`languages.toml`)

```toml
[language-server.rpm-spec-lsp]
command = "rpm-spec-lsp"

[[language]]
name = "spec"
scope = "source.rpm-spec"
file-types = ["spec"]
roots = [".rpmspec.toml", ".git"]
language-servers = ["rpm-spec-lsp"]
```

### Emacs (`eglot`, built-in since Emacs 29)

```elisp
(with-eval-after-load 'eglot
  (add-to-list 'eglot-server-programs
               '(rpm-spec-mode . ("rpm-spec-lsp"))))
(add-hook 'rpm-spec-mode-hook #'eglot-ensure)
```

### VS Code

No dedicated extension yet. Use [`vscode-generic-lsp`][generic] or any
LSP-client extension that lets you point at an arbitrary stdio binary
for the `spec` file association.

[generic]: https://marketplace.visualstudio.com/items?itemName=astro-build.vscode-generic-lsp

## Configuration

The server reads the same `.rpmspec.toml` as the CLI. It walks upward
from the opened file's directory, falling back to defaults when no
config is found. Common knobs:

```toml
# .rpmspec.toml

# Pick a distribution profile so family-gated rules behave correctly.
profile = "fedora-40"

[lints]
# Silence a noisy rule.
mixed-spaces-and-tabs = "allow"
# Fail the editor's problem list, not just warn.
missing-changelog    = "deny"

[shellcheck]
# Disable the optional shellcheck integration entirely.
disabled = true
```

See the [root README](../../README.md) for the full schema. Re-reading
the config on save is not implemented yet — restart the server (or the
editor) after editing `.rpmspec.toml`.

## Position encoding

The server advertises UTF-16 in its capabilities (the LSP 3.17
default) and accepts UTF-8 from any client that lists it under
`general.positionEncodings`. Neovim and Helix negotiate UTF-8 by
default, which skips the codepoint walk on every span conversion and
is meaningfully faster on non-ASCII specs.

## Architecture

Five thin modules sit on top of the analyzer:

```
src/
├── encoding.rs    byte ↔ Position (UTF-8/UTF-16)
├── document.rs    in-memory buffers + cached ParseOutcome + URI → PathBuf
├── diagnostics.rs analyzer::Diagnostic → lsp_types::Diagnostic
├── code_actions.rs Suggestion → CodeAction (quick fix)
├── outline.rs     SpecFile → DocumentSymbol tree
├── folding.rs     SpecFile → FoldingRange[]
├── hover.rs       static doc lookup + profile macro fallback
├── completion.rs  context-aware completion incl. profile macros
├── inlay.rs       macro expansion ghost text via Profile::expand_to_literal
├── rename.rs      macro rename: %define/%global + %name/%{name} refs
├── xref.rs        goto definition / references / documentHighlight
└── server.rs      stdio main loop + message dispatch + watcher reload
```

Rename covers user-defined macros only — `%define foo`/`%global foo`
sites and every `%foo` / `%{foo}` / `%{?foo}` / `%{!?foo}` /
`%{?foo:VALUE}` reference in the same file. Grammar keywords
(`%prep`, `%if`, …) are rejected at `prepareRename`. Sub-package
names and source/patch numbers are out of scope for the current
implementation.

The analyzer is invoked synchronously on every `didOpen` / `didChange`.
For typical spec sizes (≤ a few thousand lines) reparsing is faster
than the LSP round-trip, so there is no incremental layer.

The transport layer is [`lsp-server`][lsp-server] — same one
`rust-analyzer` uses. Message types come from [`lsp-types`][lsp-types]
0.97 (LSP 3.17).

[lsp-server]: https://crates.io/crates/lsp-server
[lsp-types]: https://crates.io/crates/lsp-types

## Testing

Integration tests spawn the server against an in-memory
`lsp-server::Connection`, send real JSON-RPC messages, and assert on
the responses — no subprocesses, no Tokio. See
[`tests/protocol.rs`](tests/protocol.rs).

```bash
cargo test -p rpm-spec-lsp
```

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](../../LICENSE-APACHE))
- MIT license ([LICENSE-MIT](../../LICENSE-MIT))

at your option.
