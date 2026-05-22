# `rpm-spec-tool` documentation

This directory contains the long-form documentation. The project
[`README.md`](../README.md) at the workspace root carries the pitch,
badges, install snippets, and a short quick-start; everything else
lives here.

## Start here

* **[Getting started](getting-started.md)** — install the tool, lint
  and reformat your first spec, learn the handful of concepts every
  later page builds on (profiles, lints, fixes, configuration).
* **[Editing workflow](workflow.md)** — the day-to-day loop for
  creating or editing a `.spec` file: edit → `pretty` → `lint`
  → `--fix` → `format` → `check` → commit. Worked example with
  screenshots.

## Reference

* **[CLI reference](cli.md)** — every top-level subcommand, its
  flags, exit codes, and intended use.
* **[Configuration (`rpmspec.toml`)](configuration.md)** — schema,
  discovery rules, and the `config init` / `validate` / `schema` /
  `doc` helpers.
* **[Lint system](lints.md)** — severity model, fix levels, category
  filters, the `lints` subcommand, and how `lints-list.md` is kept in
  sync.
* **[Lint rules catalogue](lints-list.md)** — auto-generated, one row
  per rule. **Do not edit by hand** — CI's `lints-doc-check` job
  regenerates it via `rpm-spec-tool lints --format markdown`.

## Distribution targeting

* **[Distribution profiles](profiles.md)** — what a profile is, the
  built-in catalogue (RHEL, ALT Linux, REDos, Rosa, MOSos, SLES, generic),
  layering rules, and the `profile *` subcommand tree.
* **[Release matrix and multi-profile analysis](matrix.md)** —
  `target-set`s, `matrix check`, baselines, portability, coverage,
  diff/impact/classes, contract verification, repo-aware deps.
* **[RPM repositories](repos.md)** — `repo sync` / `show` / `cache`,
  the on-disk cache, the offline-by-default policy, and how the
  repo set feeds repo-aware lints and `matrix deps`.

## Integration

* **[Editor integration (LSP)](editor-integration.md)** — `rpm-spec-lsp`
  setup for Neovim, Helix, Emacs, and VS Code; JSON-Schema completion
  for `rpmspec.toml`; shell completions for the CLI.
* **[CI integration](ci-integration.md)** — `check` for one-shot
  gating, SARIF for GitHub Code Scanning, `matrix check` with
  baselines, and how to wire `repo sync` into a CI cache.

## Conventions used in these docs

* Commands are shown as `rpm-spec-tool <subcommand>` even where the
  shell prompt is `$` or `#` — copy without the prompt.
* Examples that need a placeholder package name use `myproject.spec`
  or `acme-product.spec`. Placeholder profile / vendor names use
  `acme`, `mycompany`, `internal-mirror`.
* Built-in profile names (`rhel-9-x86_64`, `altlinux-10-x86_64`,
  `sles-15-x86_64`, …) are real identifiers — pass them verbatim.
