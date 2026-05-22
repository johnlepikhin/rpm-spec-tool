# Editor integration

`rpm-spec-tool` ships a Language Server Protocol implementation,
`rpm-spec-lsp`, that surfaces every analyzer lint as an inline
diagnostic, exposes the machine-applicable fixes as quick-fix code
actions, and powers outline / hover / completion. The same
`rpmspec.toml` the CLI reads also drives the LSP server, so the
editor and CI agree on what's a warning.

This page covers:

* installing the LSP server,
* wiring it into Neovim, Helix, Emacs, and VS Code,
* JSON-Schema completion for `rpmspec.toml`,
* shell completions for the CLI itself.

The full LSP feature matrix and protocol details are in
[`crates/lsp/README.md`](../crates/lsp/README.md).

## Install `rpm-spec-lsp`

The server is a separate binary in the same workspace:

```bash
# From a git checkout / via cargo:
cargo install --git https://github.com/johnlepikhin/rpm-spec-tool rpm-spec-lsp

# Or, from a local clone:
cargo build --release -p rpm-spec-lsp
# Binary lands in ./target/release/rpm-spec-lsp
```

It is a synchronous stdio server in the style of `rust-analyzer` — no
Tokio, one binary. Speaks LSP 3.17, negotiates UTF-8 / UTF-16
position encoding.

## What you get

| Capability                                                  | Status |
|-------------------------------------------------------------|:------:|
| `publishDiagnostics` (analyzer lints + URL)                 |   ✓    |
| `codeAction` (quick fixes — same rewrites as `lint --fix`)  |   ✓    |
| `documentSymbol` (outline)                                  |   ✓    |
| `foldingRange` (sections + `%if` blocks)                    |   ✓    |
| `hover` (tags, directives, profile macros)                  |   ✓    |
| `completion` (tags, directives, profile macros)             |   ✓    |
| `inlayHint` (macro expansion from profile)                  |   ✓    |
| `definition` (macro → `%define` site)                       |   ✓    |
| `references` (macro usages)                                 |   ✓    |
| `documentHighlight` (same-name occurrences)                 |   ✓    |
| `prepareRename` + `rename` (macros)                         |   ✓    |
| `didChangeWatchedFiles` (auto-reload `.rpmspec.toml`)       |   ✓    |
| `formatting` (LSP-driven `rpm-spec-tool format`)            |   —    |
| Subpackage / Source / Patch rename                          |   —    |
| Semantic tokens (use the editor's tree-sitter grammar)      |   —    |

Rename covers user-defined macros only — `%define foo` / `%global foo`
sites and every reference (`%foo`, `%{foo}`, `%{?foo}`, `%{!?foo}`,
`%{?foo:VALUE}`) in the same file. Grammar keywords (`%prep`, `%if`,
…) are rejected at `prepareRename`.

## Neovim

`nvim-lspconfig` ≥ 0.1.7 supports the new `vim.lsp.config` form:

```lua
vim.lsp.config('rpm_spec_lsp', {
  cmd = { 'rpm-spec-lsp' },
  filetypes = { 'spec' },
  root_dir = function(_, ctx)
    return vim.fs.root(ctx.bufnr, { '.rpmspec.toml', 'rpmspec.toml', '.git' })
           or vim.fn.getcwd()
  end,
})
vim.lsp.enable('rpm_spec_lsp')
```

For older Neovim falling back to the classic `require('lspconfig')`:

```lua
local lspconfig = require('lspconfig')
local configs   = require('lspconfig.configs')
if not configs.rpm_spec_lsp then
  configs.rpm_spec_lsp = {
    default_config = {
      cmd = { 'rpm-spec-lsp' },
      filetypes = { 'spec' },
      root_dir = lspconfig.util.root_pattern('.rpmspec.toml', 'rpmspec.toml', '.git'),
      single_file_support = true,
    },
  }
end
lspconfig.rpm_spec_lsp.setup({})
```

## Helix (`languages.toml`)

```toml
[language-server.rpm-spec-lsp]
command = "rpm-spec-lsp"

[[language]]
name = "spec"
scope = "source.rpm-spec"
file-types = ["spec"]
roots = [".rpmspec.toml", "rpmspec.toml", ".git"]
language-servers = ["rpm-spec-lsp"]
```

## Emacs (`eglot`, built-in since Emacs 29)

```elisp
(with-eval-after-load 'eglot
  (add-to-list 'eglot-server-programs
               '(rpm-spec-mode . ("rpm-spec-lsp"))))
(add-hook 'rpm-spec-mode-hook #'eglot-ensure)
```

## VS Code

There is no dedicated VS Code extension yet. Use a generic LSP-client
extension such as
[`vscode-generic-lsp`](https://marketplace.visualstudio.com/items?itemName=astro-build.vscode-generic-lsp)
and point it at `rpm-spec-lsp` for the `spec` file association.

## Position encoding

The server advertises UTF-16 in its capabilities (the LSP 3.17
default) and accepts UTF-8 from any client that lists it under
`general.positionEncodings`. Neovim and Helix negotiate UTF-8 by
default — that skips the codepoint walk on every span conversion and
is measurably faster on non-ASCII specs.

## Configuration

The server reads the same `rpmspec.toml` as the CLI via the same
cascade (`--config` if launched with one, `$RPM_SPEC_TOOL_CONFIG`,
then `$XDG_CONFIG_HOME/rpm-spec-tool/rpmspec.toml`, then built-in
defaults). Save the file, and `didChangeWatchedFiles` triggers a
reload — diagnostics refresh without restarting the server.

The schema and the four `config` subcommands are documented in
[configuration.md](configuration.md).

## TOML schema completion (for editing `rpmspec.toml`)

The CLI emits the JSON Schema for its own config file:

```bash
rpm-spec-tool config schema > .rpmspec.schema.json
```

* **taplo** (`~/.config/taplo/taplo.toml`):

  ```toml
  [[rule]]
  include = ["**/rpmspec.toml", "**/.rpmspec.toml"]
  schema  = { path = ".rpmspec.schema.json" }
  ```

* **VS Code** with the
  [Even Better TOML](https://marketplace.visualstudio.com/items?itemName=tamasfe.even-better-toml)
  extension — same `taplo.toml` rule applies, picked up automatically.

* **Helix** and **Zed** — point their TOML language server at the
  same schema path.

Validate by typing a bad key (`extends-from = "rhel"`); the editor
should immediately flag it.

## Shell completions

Generate a completion script with the `completions` subcommand:

```bash
# Bash (system-wide):
rpm-spec-tool completions bash       | sudo tee /etc/bash_completion.d/rpm-spec-tool >/dev/null

# Zsh (per-user; ensure the target dir is on $fpath):
rpm-spec-tool completions zsh        > ~/.zsh/completions/_rpm-spec-tool

# Fish (per-user):
rpm-spec-tool completions fish       > ~/.config/fish/completions/rpm-spec-tool.fish

# PowerShell:
rpm-spec-tool completions powershell >> $PROFILE
```

Supported shells: `bash`, `zsh`, `fish`, `powershell`, `elvish`. The
scripts are generated by `clap_complete` so they cover every
subcommand and flag known to the binary.

## Architecture (for the curious)

Five thin modules sit on top of the analyzer:

```
crates/lsp/src/
├── encoding.rs    byte ↔ Position (UTF-8/UTF-16)
├── document.rs    in-memory buffers + cached ParseOutcome + URI → PathBuf
├── diagnostics.rs analyzer::Diagnostic → lsp_types::Diagnostic
├── code_actions.rs Suggestion → CodeAction (quick fix)
├── outline.rs     SpecFile → DocumentSymbol tree
├── folding.rs     SpecFile → FoldingRange[]
├── hover.rs       static doc lookup + profile macro fallback
├── completion.rs  context-aware completion incl. profile macros
├── inlay.rs       macro expansion ghost text via Profile::expand_to_literal
├── rename.rs      macro rename: %define/%global + references
├── xref.rs        goto definition / references / documentHighlight
└── server.rs      stdio main loop + message dispatch + watcher reload
```

The transport layer is
[`lsp-server`](https://crates.io/crates/lsp-server) — same one
`rust-analyzer` uses. Message types come from
[`lsp-types`](https://crates.io/crates/lsp-types) 0.97. The analyzer
is invoked synchronously on every `didOpen` / `didChange`; for typical
spec sizes reparsing beats the LSP round-trip, so there's no
incremental layer.
