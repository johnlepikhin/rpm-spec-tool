# rpm-spec-analyzer

[![CI](https://github.com/johnlepikhin/rpm-spec-tool/actions/workflows/ci.yml/badge.svg)](https://github.com/johnlepikhin/rpm-spec-tool/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/rpm-spec-analyzer.svg)](https://crates.io/crates/rpm-spec-analyzer)
[![docs.rs](https://img.shields.io/docsrs/rpm-spec-analyzer)](https://docs.rs/rpm-spec-analyzer)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

Visitor-based static analyzer library powering the
[`rpm-spec-tool`](https://crates.io/crates/rpm-spec-tool) CLI. Parses
`.spec` sources with the [`rpm-spec`](https://crates.io/crates/rpm-spec)
crate, walks the resulting AST through a registry of lint rules, and
surfaces findings as `Diagnostic`s with spans, severities, and
machine-applicable fix suggestions. Profile-aware: family-gated rules
(`Fedora`, `Rhel`, `Opensuse`, `Alt`, `Mageia`) consult the resolved
[`rpm-spec-profile::Profile`](https://crates.io/crates/rpm-spec-profile)
to know which idioms to enforce.

## Quick start

```rust
use rpm_spec_analyzer::{analyze, Severity};
use rpm_spec_analyzer::config::Config;

let source = r#"
Name: foobar
Version: 1.0
Release: 1%{?dist}
Summary: Example package
License: MIT

%description
An example spec.

%changelog
"#;

let config = Config::default();
let (outcome, diagnostics) = analyze(source, &config);

// Parser-level findings (recoverable parse issues) come back inside
// `outcome.parser_diagnostics`; lint findings are in `diagnostics`.
for diag in &diagnostics {
    let marker = match diag.severity {
        Severity::Deny => "error",
        Severity::Warn => "warning",
        Severity::Allow => continue, // never emitted
    };
    println!(
        "{marker}: [{id}] {msg} at byte {start}..{end}",
        id = diag.lint_id,
        msg = diag.message,
        start = diag.primary_span.start_byte,
        end = diag.primary_span.end_byte,
    );
}

let _ = outcome; // `outcome.spec` is the parsed AST for downstream tooling.
```

To run profile-aware rules, resolve a profile via `rpm-spec-profile`
and call `analyze_with_profile(source, &config, profile)` instead.

## Lint catalogue

The full catalogue of built-in rules — IDs, default severities,
categories, one-line descriptions — lives in
[`doc/lints-list.md`](../../doc/lints-list.md) in the workspace root.
That file is regenerated from the rule registry and grouped by
category (correctness, packaging, style, performance).

## Configuration

Lint behaviour is driven by `rpm_spec_analyzer::config::Config`:

- `lints: BTreeMap<String, Severity>` — per-lint severity overrides
  (`allow` / `warn` / `deny`), keyed by kebab-case lint name.
- `profile: Option<String>` plus `profiles: BTreeMap<String, ProfileEntry>` —
  the active distribution profile and any user-defined entries.
- `shellcheck: ShellcheckConfig` — binary path, per-`SC` enable / disable
  lists, dialect, timeout for the optional shellcheck integration.
- `warnings_as_errors: bool` — promote every `Warn` finding to `Deny`
  at runtime.

For the user-facing `.rpmspec.toml` schema (the canonical way to
populate `Config` for library consumers that don't want to build it
manually), see the [root README](../../README.md).

## Re-exported types

The public surface lives at the crate root: `analyze`,
`analyze_with_profile`, `analyze_with_profile_at`, `parse`,
`LintSession`, `ParseOutcome`, `Diagnostic`, `Severity`, `Label`,
`Edit`, `Suggestion`, `Applicability`, `LintCategory`, `Lint`,
`LintMetadata`, `Visit`. The `rpm-spec-profile` crate is re-exported as
`rpm_spec_analyzer::profile` for convenience.

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
