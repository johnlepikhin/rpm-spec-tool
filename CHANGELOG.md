# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

### Changed

### Fixed

## [0.1.0] - 2026-05-16

### Added

- Initial release of the `rpm-spec-tool` workspace, organised into three
  published crates: `rpm-spec-profile` (distribution profile model and macro
  registry), `rpm-spec-analyzer` (visitor-based static analyzer library), and
  `rpm-spec-tool` (the CLI binary).
- Added the `lint` subcommand with rule-based static analysis over the parsed
  AST, `codespan`-style diagnostics, machine-applicable autofixes (`--fix`,
  `--fix-suggested` for "maybe-incorrect" rewrites), and per-rule severity
  overrides via `--deny` / `--warn` / `--allow` flags or `.rpmspec.toml`
  defaults.
- Added the `format` and `pretty` subcommands for canonical reformatting,
  including `--check` (dry-run for CI), `--in-place` (overwrite), and
  `--diff` (unified-diff) modes; `pretty` streams ANSI syntax-highlighted
  output to stdout.
- Added SARIF diagnostic output (`lint --format sarif`) for GitHub Code
  Scanning, alongside human-readable text and JSON formats.
- Added 24 built-in distribution profiles (generic, RHEL 8 / 9 / 10,
  Fedora-derived families, SUSE, ALT Linux variants) selecting the right
  macro registry, rpmlib feature set, and family-gated lints; selectable via
  `--profile <name>` or `.rpmspec.toml`, and inspectable through the
  `profile show`, `profile list`, and `profile macro` subcommands.
- Added `rpmbuild`-style `--define` / `-D 'NAME VALUE'` macro injection for
  `lint`, `check`, and `profile *` subcommands; repeatable, with CLI defines
  outranking profile and config defaults.
- Added optional [`shellcheck`](https://www.shellcheck.net/) integration for
  `%prep`, `%build`, and `%install` script sections, configurable via the
  `[shellcheck]` table in `.rpmspec.toml`; a missing `shellcheck` binary is
  surfaced as a separate diagnostic and never crashes the lint.
- Added the `ast` subcommand for dumping the parsed AST as JSON or YAML for
  downstream tooling.
- Added the `check` subcommand for one-shot CI gating that combines lint and
  format-check and exits non-zero on any issue.
- `completions <SHELL>` subcommand — emits a `clap_complete`-generated
  completion script for `bash`, `zsh`, `fish`, `powershell`, or `elvish`
  to stdout.
- Added a GitHub Actions CI workflow and a release workflow that publishes
  `.tar.gz`, `.deb`, and `.rpm` artifacts (plus `SHA256SUMS`) for Linux
  `x86_64` and `aarch64` on each `vX.Y.Z` tag.

[Unreleased]: https://github.com/johnlepikhin/rpm-spec-tool/compare/v0.1.0...HEAD
[0.1.0]:      https://github.com/johnlepikhin/rpm-spec-tool/releases/tag/v0.1.0
