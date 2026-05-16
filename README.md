# rpm-spec-tool

[![CI](https://github.com/johnlepikhin/rpm-spec-tool/actions/workflows/ci.yml/badge.svg)](https://github.com/johnlepikhin/rpm-spec-tool/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/rpm-spec-tool.svg)](https://crates.io/crates/rpm-spec-tool)
[![docs.rs](https://img.shields.io/docsrs/rpm-spec-tool)](https://docs.rs/rpm-spec-tool)
[![MSRV](https://img.shields.io/crates/msrv/rpm-spec-tool.svg?label=msrv)](https://www.rust-lang.org/)
[![dependency status](https://deps.rs/repo/github/johnlepikhin/rpm-spec-tool/status.svg)](https://deps.rs/repo/github/johnlepikhin/rpm-spec-tool)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

Pretty-printer and static analyzer CLI for RPM `.spec` files.

Built on top of the [`rpm-spec`](https://crates.io/crates/rpm-spec) parser and a
visitor-based analyzer that ships with 24 built-in distribution profiles
(generic, RHEL 8/9/10, Fedora-derived families, SUSE, ALT Linux variants).

<p align="center">
  <a href="https://raw.githubusercontent.com/johnlepikhin/rpm-spec-tool/main/doc/images/linter.png">
    <img src="https://raw.githubusercontent.com/johnlepikhin/rpm-spec-tool/main/doc/images/linter.png"
         alt="rpm-spec-tool lint sample output — codespan-style diagnostics with cross-references and suggestions"
         width="820">
  </a>
  <br>
  <sub><i>Sample <code>rpm-spec-tool lint</code> output: boolean and conditional simplifications, <code>bcond</code> hygiene, metadata consistency &mdash; with cross-referenced spans and machine-applicable fixes.</i></sub>
</p>

## Features

- **Static analysis** — lint rules over the parsed AST with rich
  `codespan`-style diagnostics. Output as human-readable text, JSON, or
  [SARIF](https://sarifweb.azurewebsites.net/) for GitHub Code Scanning.
- **Auto-fixes** — `lint --fix` applies machine-applicable rewrites in place;
  `--fix-suggested` also commits "maybe-incorrect" rewrites.
- **Severity overrides** — `--deny`, `--warn`, `--allow` on the CLI; per-lint
  defaults in `.rpmspec.toml`.
- **Pretty-printer** — `format` for canonical reformat (with `--check`,
  `--in-place`, `--diff` modes for CI / pre-commit); `pretty` streams the
  reformatted text to stdout with ANSI syntax highlighting.
- **Distribution profiles** — 24 built-in profiles select the right macro
  registry, rpmlib feature set, and family-gated lints. Pick one with
  `--profile <name>` or via `.rpmspec.toml`.
- **`--define` like `rpmbuild`** — `-D 'NAME VALUE'` injects macros at lint
  time; CLI defines outrank profile / config defaults. Repeatable.
- **Shellcheck integration** — optional. The analyzer invokes
  [`shellcheck`](https://www.shellcheck.net/) on `%prep`, `%build`, `%install`
  script sections and surfaces its findings; missing `shellcheck` in `$PATH`
  is reported as a separate diagnostic, never crashes the lint. Configurable
  via `[shellcheck]` in `.rpmspec.toml`.
- **AST dump** — `ast` emits the parsed AST as JSON or YAML for downstream
  tooling.

<table align="center">
  <tr>
    <td align="center">
      <a href="https://raw.githubusercontent.com/johnlepikhin/rpm-spec-tool/main/doc/images/pretty.png">
        <img src="https://raw.githubusercontent.com/johnlepikhin/rpm-spec-tool/main/doc/images/pretty.png"
             alt="rpm-spec-tool pretty sample output — indented and syntax-highlighted .spec rendering"
             width="380">
      </a>
    </td>
    <td align="center">
      <a href="https://raw.githubusercontent.com/johnlepikhin/rpm-spec-tool/main/doc/images/format-diff.png">
        <img src="https://raw.githubusercontent.com/johnlepikhin/rpm-spec-tool/main/doc/images/format-diff.png"
             alt="rpm-spec-tool format --diff sample output — coloured unified diff against the canonical form"
             width="380">
      </a>
    </td>
  </tr>
  <tr>
    <td align="center"><sub><i><code>pretty</code> &mdash; indented, syntax-highlighted view for reading.</i></sub></td>
    <td align="center"><sub><i><code>format --diff</code> &mdash; unified diff for CI / pre-commit gating.</i></sub></td>
  </tr>
</table>

<p align="center">
  <a href="https://raw.githubusercontent.com/johnlepikhin/rpm-spec-tool/main/doc/images/profile-macro.png">
    <img src="https://raw.githubusercontent.com/johnlepikhin/rpm-spec-tool/main/doc/images/profile-macro.png"
         alt="rpm-spec-tool profile macro sample output — comparison of a single macro across distribution profiles"
         width="640">
  </a>
  <br>
  <sub><i><code>profile macro</code> compares a single macro across distributions &mdash; the same logical path resolves to three different idioms.</i></sub>
</p>

## Quick start

```sh
# Lint, with a chosen distribution profile:
rpm-spec-tool lint --profile rhel-9-x86_64 my-package.spec

# Apply auto-fixes, then reformat in place:
rpm-spec-tool lint   --fix --profile rhel-9-x86_64 my-package.spec
rpm-spec-tool format --in-place                    my-package.spec

# CI: one-shot lint + format check (exits non-zero on any issue):
rpm-spec-tool check --profile rhel-9-x86_64 my-package.spec

# SARIF output for GitHub Code Scanning:
rpm-spec-tool lint --format sarif my-package.spec > spec.sarif

# Inspect a built-in profile:
rpm-spec-tool profile show rhel-9-x86_64
rpm-spec-tool profile list

# Compare a macro across profiles (note: macro name without `%`):
rpm-spec-tool profile macro dist rhel-9-x86_64 rhel-10-x86_64
```

Every spec-taking subcommand reads from stdin when the path is `-` or omitted.

## Configuration

Drop a `.rpmspec.toml` next to your specs (or anywhere up the directory tree —
the tool walks upward from each input). Minimal example:

```toml
# Pick the active distribution profile. CLI `--profile` overrides this.
profile = "rhel-9-x86_64"

# Per-lint severity overrides. CLI flags --deny/--warn/--allow win.
# Rule IDs below are illustrative — see the lint reference for the full list.
[lints]
RPM031 = "warn"
RPM093 = "allow"

# Tune shellcheck. Set `shellcheck = "allow"` at the top level to
# disable shellcheck entirely.
[shellcheck]
binary       = "shellcheck"
timeout_secs = 10

# Define macros at lint time (also available via CLI: -D 'name value').
[profiles."rhel-9-x86_64".macros]
mybranch = "production"
```

Subcommand options that override the config:

| Flag | Available in | Purpose |
| --- | --- | --- |
| `--config <PATH>` | `lint`, `check`, `format`, `pretty`, `profile *` | explicit `.rpmspec.toml` path |
| `--profile <NAME>` | `lint`, `check` | pick the distribution profile (in `profile show` the name is a positional argument) |
| `-D, --define 'NAME VALUE'` | `lint`, `check`, `profile *` | inject a macro (rpmbuild-style); repeatable |
| `--deny <LINT>` / `--warn` / `--allow` | `lint` | override severity for a single rule; repeatable |
| `--fix` / `--fix-suggested` | `lint` | apply machine-applicable (and optionally suggested) rewrites |
| `--format human\|json\|sarif` | `lint` | diagnostics output format |
| `--check` / `--in-place` / `--diff` | `format` | dry-run / overwrite / unified-diff modes |
| `--indent N` | `format`, `pretty` | indent `%if` blocks for readability (rpm rejects indented `%if` — display only) |
| `--format json\|yaml` / `--pretty` | `ast` | AST dump format |
| `--color auto\|always\|never` | top-level and every subcommand | ANSI colour control |

## Exit codes

| Command | Code | Meaning |
| --- | --- | --- |
| `lint` | `0` | no errors (warnings allowed) |
| `lint` | `1` | one or more error-severity diagnostics |
| `format --check` | `0` | input is already canonical |
| `format --check` | `1` | input would be rewritten |
| `check` | `1` | either lint or format-check failed |
| `profile macro <NAME>` | `2` | single-profile lookup with undefined macro |
| any | non-zero | parse error, I/O failure, or invalid CLI |

## Lint rules

The catalogue of available lints (rule IDs, severities, autofixes, examples)
will live in a dedicated reference document — _coming soon: `doc/lints.md`_.

The distribution-profile model is documented in
[`doc/profiles.md`](doc/profiles.md).

## Installation

### Pre-built packages

Each tag `vX.Y.Z` publishes `.tar.gz`, `.deb`, and `.rpm` for Linux `x86_64`
and `aarch64` (plus a `SHA256SUMS` file) at
[github.com/johnlepikhin/rpm-spec-tool/releases](https://github.com/johnlepikhin/rpm-spec-tool/releases).

```sh
# Debian / Ubuntu derivatives:
sudo apt install ./rpm-spec-tool_X.Y.Z-1_amd64.deb

# RHEL / Fedora derivatives:
sudo dnf install ./rpm-spec-tool-X.Y.Z-1.x86_64.rpm
```

`shellcheck` is declared as a `Recommends` / `Suggests` dependency — install
it if you want the shell-section lints to run.

### From source

Requires Rust **1.88** or newer.

```sh
# Latest from git:
cargo install --git https://github.com/johnlepikhin/rpm-spec-tool rpm-spec-tool

# From a local clone:
cargo install --path crates/cli
```

### Shell completions

After installation, generate a completion script for your shell with the
`completions` subcommand:

```sh
# Bash (system-wide):
rpm-spec-tool completions bash | sudo tee /etc/bash_completion.d/rpm-spec-tool >/dev/null

# Zsh (per-user; ensure the target dir is on $fpath):
rpm-spec-tool completions zsh  > ~/.zsh/completions/_rpm-spec-tool

# Fish (per-user):
rpm-spec-tool completions fish > ~/.config/fish/completions/rpm-spec-tool.fish

# PowerShell:
rpm-spec-tool completions powershell >> $PROFILE
```

Supported shells: `bash`, `zsh`, `fish`, `powershell`, `elvish`.

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
