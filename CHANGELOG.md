# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

### Changed

### Fixed

## [0.1.3] - 2026-05-21

### Fixed

- Release binaries previously built on Ubuntu 24.04 baked glibc
  2.39 into the resulting ELF, breaking on Debian 12 (glibc 2.36)
  and other distros pinned at glibc 2.35–2.37. The release
  workflow now builds on `ubuntu-22.04` / `ubuntu-22.04-arm`
  (glibc 2.35), restoring portability to every distro shipping
  glibc ≥ 2.35.

## [0.1.2] - 2026-05-21

### Added

- `config init` defaults the output path to the XDG config location
  (`$XDG_CONFIG_HOME/rpm-spec-tool/rpmspec.toml`, typically
  `~/.config/rpm-spec-tool/rpmspec.toml`) — the same file the loader
  picks up. Parent directory is created if missing.
- `repo cache gc` / `repo cache prune` and `config init --force` now
  prompt for confirmation on a TTY (`[y/N]`, abort exits 130) and
  refuse on non-TTY without an explicit `--yes`. A `--dry-run` flag
  on every destructive subcommand previews the actions without
  touching disk.
- `lint --fix` and `format --check` print end-of-run summaries
  ("applied N fixes across M files", "all N files are properly
  formatted", "no files to check", …).
- `output/mod.rs::resolve_color` honours the `NO_COLOR` env var
  (no-color.org standard) and `CLICOLOR_FORCE`, with the precedence
  explicit-flag > CLICOLOR_FORCE > NO_COLOR > TTY detection.
- `matrix impact --from REV` differentiates "not a git repo" from
  "revision not found" and suggests `git log --oneline` in the
  hint.
- `repo cache list` is now exposed as a visible alias for
  `repo cache ls`.
- `completions --help` prepends a one-line explanation that the
  command emits a script to stdout before the per-shell installation
  examples.

### Changed

- Bumped `rpm-spec` to 0.4.1 and dropped the `[patch.crates-io]`
  override — the released crate carries the printer fixes (parser
  `%%{X}` decoding, scriptlet/`%description`/`%prep` body
  indentation stability) that were previously local-only.
- `config init --help`'s generated header trimmed from 15 to 6
  lines; the discovery cascade explanation moved out of every
  generated config to `--help` text.
- Top-level command descriptions (`lint`, `format`, `pretty`,
  `ast`, `check`, `profile`, `target`, `matrix`, `repo`, `lints`,
  `config`, `completions`) rewritten to a uniform single-line
  imperative voice.
- `repo sync` progress messages routed to stderr so that stdout
  stays clean for structured pipes; phrased as narrative
  ("syncing X/Y (K) U", "→ revision R, N packages …") instead of
  the log-style `key=value` form.
- Matrix human renderer dropped markdown leakage (`# Matrix run`
  → `Matrix run:`, `## file` → `=== file ===`) so the output no
  longer collides with `git diff` markers in mixed contexts.
- `RPM050` (`hardcoded-paths`) now skips comments, paths prefixed
  by `%{buildroot}` / `$RPM_BUILD_ROOT`, and scriptlet / trigger
  bodies. Workspace QA-corpus diagnostic count dropped from 372
  to 235 (~43%) without losing real findings.
- `--define NAME=VALUE` (the wrong shape — rpmbuild uses
  whitespace, not `=`) now emits a friendly two-line error with a
  hint instead of failing deep in the macro resolver.
- Workspace lint config (`[workspace.lints.clippy]` in
  `Cargo.toml`): `collapsible_if`, `collapsible_match`,
  `collapsible_else_if`, `unnecessary_sort_by` raised to `deny`.
  Local clippy on rustc ≥ 1.95 now mirrors CI.

### Fixed

- **`config validate --config <PATH>` silently ignored the global
  flag.** Plumbed `config_override` through `Application::run` →
  `config::Cmd::run` → `validate::run`; resolution now honours the
  documented cascade (positional → `--config` → env var → XDG →
  upward walk). The error message split into `error:` + `hint:`
  lines naming all override paths.
- `lint --fix` and `format --check` are no longer silent on the
  zero-changes case; both report it explicitly.
- `format --in-place` on stdin now emits a clean error (exit 2)
  instead of silently discarding the formatted output.
- `matrix check`'s `--warn` and `--allow` flags now carry the same
  `--help` documentation as `--deny` (previously empty).
- `repo cache prune --repo ' '` (whitespace-only prefix) is
  rejected instead of silently matching nothing; `repo cache gc
  --dry-run` and `prune --dry-run` print `(no snapshots to
  remove)` / `(no repos to prune)` when the candidate list is
  empty.
- `repo cache` and `config init` use exit code 2 consistently for
  refused destructive actions and 130 for user-aborted prompts.
- 50+ `clippy::collapsible_if` / `collapsible_match` sites
  flagged by rustc 1.95 collapsed to let-chains or guard patterns;
  `if-let-guard` cases annotated with `#[allow(...)]` until that
  feature stabilises.
- `repo cache gc/prune` snapshot ordering switched from
  `sort_by(|a, b| b.1.cmp(&a.1))` to `sort_by_key(|e|
  Reverse(e.1))` for `clippy::unnecessary_sort_by` compliance.
- `doc/lints-list.md` regenerated to match the current rule
  catalogue (reordering + a handful of new descriptions).

## [0.1.1] - 2026-05-16

### Changed

- Removed the `publish-crates` job from the release workflow. Releases
  to crates.io are now done manually from a local checkout; the
  GitHub Release with binary artifacts continues to ship on every
  `vX.Y.Z` tag.

### Fixed

- Multiple `clippy::collapsible-if`, `clippy::collapsible-match`, and
  `clippy::unnecessary-sort-by` errors flagged by rust-1.95's stricter
  clippy across the analyzer rules and the profile auto-detect path.
  Pure mechanical refactors (`if let` pairs collapsed to let-chains
  via the 2024-edition syntax; `sort_by` replaced with
  `sort_by_key(|e| Reverse(e.len()))`; nested `if`-in-match-arm
  folded into guards). No semantic change; the full test suite
  (1230 tests) is unaffected.
- Replaced the manual `impl Default for ValidationMode` with a derive
  + `#[default]` variant, satisfying `clippy::derivable-impls`.

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

[Unreleased]: https://github.com/johnlepikhin/rpm-spec-tool/compare/v0.1.3...HEAD
[0.1.3]:      https://github.com/johnlepikhin/rpm-spec-tool/compare/v0.1.2...v0.1.3
[0.1.2]:      https://github.com/johnlepikhin/rpm-spec-tool/compare/v0.1.1...v0.1.2
[0.1.1]:      https://github.com/johnlepikhin/rpm-spec-tool/compare/v0.1.0...v0.1.1
[0.1.0]:      https://github.com/johnlepikhin/rpm-spec-tool/releases/tag/v0.1.0
