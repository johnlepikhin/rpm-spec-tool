# Lint system

The analyzer ships with a registry of built-in rules, each with a
default severity and a category. This page covers the model — how
severities resolve, what fixes the analyzer can emit, how to filter
the catalogue, and where the auto-generated rule list lives.

For a single concrete rule's behaviour and examples, look it up in
[lints-list.md](lints-list.md). For the CLI surface that runs them,
see [cli.md § lint](cli.md#lint).

## Severity model

Every rule has a *default* severity baked into its registry entry.
Default severities are split three ways:

| Severity | Reported? | Exit code 1? | Use |
| -------- | --------- | ------------ | --- |
| `allow`  | no        | no           | the rule is silenced by default. Included in the registry so users can ask "what's off?". |
| `warn`   | yes       | no           | reported but doesn't gate the run. |
| `deny`   | yes       | **yes**      | reported *and* makes `lint` / `check` exit `1`. |

The active severity for each rule is the **last write** in this
precedence ladder:

1. Built-in default in the registry.
2. `[lints]` table in `rpmspec.toml` — keys are rule IDs (`RPM031`)
   or short names (`missing-changelog`); values are
   `"allow" | "warn" | "deny"`.
3. CLI overrides: `--deny LINT`, `--warn LINT`, `--allow LINT`.
   Repeatable; CLI always wins.

Two special meta-names — clippy convention:

* `--deny warnings` promotes **every** `warn` rule to `deny`, useful
  in CI to gate on any warning while still allowing `--allow LINT` to
  silence specific rules individually.
* `--allow warnings` clears any earlier `--deny warnings`.

Example — promote everything to deny, then silence two known-noisy
rules:

```bash
rpm-spec-tool lint --deny warnings --allow RPM093 --allow RPM200 \
  --profile rhel-9-x86_64 myproject.spec
```

## Fix levels

Some rules ship machine-applicable rewrites. The analyzer rates each
rewrite at one of two levels:

* **Safe** — mechanical and provably preserves behaviour. Examples:
  collapse `cp + chmod` to `install -m`, normalise weird whitespace in
  `Requires:` lists.
* **Suggested** (maybe-incorrect) — best-effort rewrite that needs a
  human read-over. Examples: deleting an apparently-unused subpackage,
  inferring `%{?_isa}` qualifiers, replacing literal arch checks with
  modern `%bcond`.

CLI behaviour:

* `--fix` (default safe-only) — applies the **safe** rewrites in place.
* `--fix --fix-suggested` — also commits suggested rewrites.

After any `--fix` run, re-run `lint` (without `--fix`) and inspect the
diff. The fix counter line — `lint --fix: applied N fixes across M
files` or `lint --fix: no fixable issues found` — is printed on stderr
in `human` mode only.

The LSP server (see [editor-integration.md](editor-integration.md))
exposes the same rewrites as quick-fix code actions, one per
diagnostic.

## Categories

Every rule belongs to exactly one category. Use them to narrow the
catalogue:

| Category      | Scope |
| ------------- | ----- |
| `style`       | Visual / stylistic conventions (whitespace, alignment, ordering). |
| `correctness` | Likely defects: missing fields, redefinitions, contradictions. |
| `packaging`   | Packaging conventions: changelog, sections, dependencies. |
| `performance` | Build / install / runtime cost. |

```bash
rpm-spec-tool lints --category correctness
rpm-spec-tool lints --category correctness --severity deny
rpm-spec-tool lints --severity allow                 # what's currently off?
```

`--category` and `--severity` are repeatable. Values **inside one
flag** OR-combine; **distinct flags** AND-combine. So
`--category correctness --category packaging --severity warn` lists
warn-severity rules in either category.

## Family-gated rules

Some lints are only meaningful on a particular distribution family
(`fedora`, `rhel`, `opensuse`, `alt`, `mageia`). These stay silent on
profiles whose `identity.family` doesn't match. The default `generic`
profile gates **all** family-aware rules off — that's why running with
no `--profile` skips them.

Run `rpm-spec-tool profile show <NAME>` to see the active family. The
family flag is auto-detected from `rpm --showrc` for the built-in
profiles; for the rest it's pinned in `data/<name>.toml`. Override
explicitly via `[profiles.X.identity]`:

```toml
[profiles.acme-build.identity]
family = "rhel"
```

## Output formats

The `lint` subcommand emits one of three formats — pick with
`--format`:

* **`human`** (default) — `codespan`-style diagnostics with primary
  and secondary spans, ANSI colour, machine-applicable fix markers.
* **`json`** — structured per-spec diagnostics for downstream tooling.
* **`sarif`** — SARIF 2.1.0, drops directly into GitHub Code Scanning.

For matrix runs the `matrix check` shape differs (per-profile
aggregated by signature) — see
[matrix.md § Output formats](matrix.md#output-formats).

## The `lints` subcommand and the registry

```bash
rpm-spec-tool lints                                 # text, grouped by category
rpm-spec-tool lints --format markdown               # the canonical reference
rpm-spec-tool lints --category correctness --severity deny
```

`--format markdown` is the source of truth for
[lints-list.md](lints-list.md). The file is regenerated:

* automatically by a versioned **pre-commit hook**
  ([`.githooks/pre-commit`](../.githooks/)) whenever staged files
  touch `crates/analyzer/src/rules/**` or the registry. Enable per
  clone with `git config core.hooksPath .githooks`.
* in CI by the `lints-doc-check` job, which fails the build when the
  committed copy drifts. PRs that bypass the hook still get caught.

**Do not edit `lints-list.md` by hand.** Edit the rule's registry
entry (`crates/analyzer/src/registry.rs`) or its source file under
`crates/analyzer/src/rules/`, then regenerate:

```bash
cargo run --release -p rpm-spec-tool -- lints --format markdown > doc/lints-list.md
```

## Shellcheck integration (`RPM200`)

The analyzer optionally invokes
[`shellcheck`](https://www.shellcheck.net/) on the `%prep`, `%build`,
`%install` sections and on scriptlet / trigger bodies. Findings
surface as `RPM200` diagnostics. The integration is **opt-out**:

* A missing `shellcheck` binary surfaces as a separate diagnostic —
  the lint never crashes.
* Tune behaviour via the `[shellcheck]` table in `rpmspec.toml`:

```toml
[shellcheck]
binary       = "shellcheck"
timeout_secs = 10
dialect      = "bash"
disable      = ["SC2086"]   # silence these in addition to the baseline
enable       = ["SC2164"]   # re-enable a baseline-silenced check
```

Silence the whole integration with `[lints] RPM200 = "allow"` (or
the equivalent CLI `--allow RPM200`).

## Programmatic consumers

* `lint --format json` and `--format sarif` produce
  schema-stable shapes — additive new fields are guarded with
  `#[serde(skip_serializing_if)]` so existing consumers don't break.
* `ast --format json|yaml` exposes the parsed AST for tooling that
  wants to bypass the registry and implement its own checks.
* The `rpm-spec-analyzer` Rust crate (`crates/analyzer/`) is the
  library entry point — `analyze_with_profile_at` is what `lint` and
  `check` call internally.

## Authoring or adjusting a rule

This page is about *using* the lint system; the
[`crates/analyzer/`](../crates/analyzer/) crate documents the rule
authoring contract. The short version:

1. New file under `crates/analyzer/src/rules/`.
2. Register it in `crates/analyzer/src/registry.rs` with ID, name,
   category, default severity, description.
3. Run `cargo test -p rpm-spec-analyzer` — the registry's invariants
   (unique ID, no severity drift between code and registry) are
   asserted in tests.
4. Stage your changes and commit. If you enabled the pre-commit hook,
   it'll regenerate `doc/lints-list.md` for you; otherwise CI's
   `lints-doc-check` will tell you to.
