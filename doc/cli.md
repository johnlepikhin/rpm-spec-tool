# CLI reference

One section per top-level subcommand. Every command also accepts the
global flags described in [§ Global flags](#global-flags) below, so
those aren't repeated under each subcommand.

For a short narrative tour of the five most-used commands see
[getting-started.md](getting-started.md). For the lint system in
detail (severity, fix levels, category filters) see
[lints.md](lints.md). For everything `rpmspec.toml` see
[configuration.md](configuration.md).

## Global flags

| Flag | Scope | Purpose |
| ---- | ----- | ------- |
| `--color auto\|always\|never` | every subcommand | ANSI colour policy; default `auto` follows TTY detection. |
| `--config <PATH>`             | every subcommand | Explicit `rpmspec.toml` path. Without it, the tool checks `$RPM_SPEC_TOOL_CONFIG`, then `$XDG_CONFIG_HOME/rpm-spec-tool/rpmspec.toml`, then walks upward from each input. |

Every command that processes spec files reads from **stdin** when the
path is `-` or omitted, and accepts any number of paths for batch
processing.

## Exit codes — at a glance

| Command                       | Code | Meaning |
| ----------------------------- | ---- | ------- |
| `lint`                        | `0`  | clean (warnings allowed) |
| `lint`                        | `1`  | one or more `deny`-severity diagnostics |
| `format --check`              | `0`  | input is canonical |
| `format --check`              | `1`  | input would be rewritten |
| `check`                       | `1`  | lint deny **or** would-reformat |
| `matrix check`                | `1`  | deny finding on at least one profile (or "new" w/ `--baseline`) |
| `matrix portability --fail-on` | `1` | macro status reaches the chosen threshold |
| `profile macro <NAME> <P>`    | `2`  | macro undefined in the single profile |
| `repo sync`                   | `1`  | per-repo failure (override with `--keep-going`) |
| any                           | `2`  | parse error, CLI misuse, I/O failure |

## `lint`

Run all active lint rules against one or more spec files.

```text
rpm-spec-tool lint [PATHS...]
    [--profile NAME]
    [--define 'NAME VALUE']...
    [--deny LINT]... [--warn LINT]... [--allow LINT]...
    [--fix] [--fix-suggested]
    [--format human|json|sarif]
```

Highlights:

* `--profile NAME` wins over the `profile = …` key in `rpmspec.toml`.
* `--deny warnings` (clippy convention) promotes every `warn` to
  `deny`; `--allow warnings` clears it.
* `--define 'NAME VALUE'` mirrors `rpmbuild --define`. Pass repeatedly
  for multiple macros. CLI defines outrank both the profile and the
  config's `[profiles.X.macros]`.
* `--fix` rewrites the file with **safe** rewrites only;
  `--fix-suggested` also commits maybe-incorrect rewrites. See
  [lints.md](lints.md#fix-levels) for the contract.
* `--format json` and `--format sarif` switch to machine-readable
  output. SARIF drops directly into GitHub Code Scanning.

The fix counter is reported on stderr in `human` mode only — `json` and
`sarif` consumers parse stdout, but the human-readable progress lines
go to stderr in any mode.

## `format`

Pretty-print one or more spec files in canonical form, writing
RPM-compatible output.

```text
rpm-spec-tool format [PATHS...]
    [--check | --in-place | --diff]
    [--preamble-align-column N]
    [--indent N]
```

* `--check` — exit 1 if any file would change; do not write.
* `--in-place` — overwrite files in place. Refuses stdin (no file to
  write back to).
* `--diff` — print a coloured unified diff against the canonical form.
* Without any of the above, the formatted spec is streamed to stdout.

`--indent N` indents `%if`/`%else`/`%endif` blocks by N spaces per
level. **Cosmetic only** — `rpm` rejects indented `%if` directives.
The command emits a stderr warning once per invocation when N > 0; use
it for review, never for commits.

`[format]` in `rpmspec.toml` provides defaults for both knobs.

## `pretty`

Streams the canonical spec to stdout with ANSI syntax highlighting and
`--indent` defaulted to `2`. Strictly a **display** mode — `pretty` is
not a round-trip target, and the indented output should never reach the
repository.

```text
rpm-spec-tool pretty [PATHS...]
    [--preamble-align-column N]
    [--indent N]
```

Pipe through `less -R` to keep colour when paging.

## `ast`

Dump the parsed AST for downstream tooling (custom checks, AST diff,
metric extraction, …).

```text
rpm-spec-tool ast [PATH] [--format json|yaml] [--pretty]
```

Reads stdin when `PATH` is `-` or omitted. The schema follows the
public Rust types in the `rpm-spec` parser crate.

## `check`

`lint` + `format --check` in one invocation. The CI shorthand when you
only need a single profile.

```text
rpm-spec-tool check [PATHS...]
    [--profile NAME]
    [--define 'NAME VALUE']...
```

Exit codes per the table at the top of this page. There's no `--fix`
here on purpose — `check` is a gate, not a mutator.

## `profile`

Inspect the resolved distribution profile and the macro registry. Full
profile model in [profiles.md](profiles.md).

```text
rpm-spec-tool profile show   [NAME] [--full] [--define 'NAME VALUE']...
rpm-spec-tool profile list   [--builtin-only | --user-only]
rpm-spec-tool profile macros [PROFILE] [--filter SUBSTR] [--source builtin|showrc|override]
rpm-spec-tool profile macro  <NAME> [PROFILES...]
rpm-spec-tool profile common [PROFILES...] [--mode existence|value] [--filter SUBSTR]
```

* `profile show --full` lists every macro with provenance
  (`builtin:<name>`, `showrc:-13`, `override`).
* `profile macro NAME` mode depends on arg count: zero ⇒ table across
  every profile; one ⇒ compact single value (exit `2` if undefined);
  two or more ⇒ comparison table.
* `profile common` requires at least two profiles. `--mode value` adds
  the requirement that bodies match — the narrow slice of truly
  portable defaults.

## `target`

Inspect the `[targets.<name>]` blocks declared in `rpmspec.toml`. See
the schema in [matrix.md](matrix.md#targetsname-schema).

```text
rpm-spec-tool target list
rpm-spec-tool target show <NAME>
```

## `matrix`

Multi-profile analysis. The full surface is documented in
[matrix.md](matrix.md); the table below indexes the subcommands.

| Subcommand | Purpose |
| ---------- | ------- |
| `matrix check`           | Lint every profile in a target set; aggregate findings. |
| `matrix baseline create` | Snapshot the current findings into a JSON baseline (for `--baseline`). |
| `matrix portability`     | Which referenced macros are defined on which profiles. |
| `matrix coverage`        | Per-branch verdict (active / dead / conditional / indeterminate) across profiles. |
| `matrix explain`         | Why is line N or macro X behaving differently across profiles. |
| `matrix expand`          | Spec source per profile with each `%if` line tagged. |
| `matrix diff`            | Binary structural diff between exactly two profiles. |
| `matrix impact`          | Per-profile dependency delta between two git revisions. |
| `matrix classes`         | Collapse a target set into dep-equivalent classes (minimal build set). |
| `matrix verify-contract` | Per-profile must-have / must-not-have BuildRequires assertions. |
| `matrix deps check`      | Repo-aware: every `BuildRequires` / `Requires` resolves against the cached metadata. |
| `matrix deps explain`    | Tree-shaped narration of the resolver's unsat core. |
| `matrix buildroot solve` | Full chroot closure per profile. |
| `matrix buildroot diff`  | Buildroot closures across two profiles. |
| `matrix upgrade-sim`     | Proposed NEVR vs what is currently in the repo per profile. |

Either `--target-set <NAME>` or `--profiles A,B,C` selects the matrix
on every command above; they are mutually exclusive. Ad-hoc
`--profiles` skips the config, useful for one-off checks.

Repo-aware subcommands (`matrix deps *`, `matrix buildroot *`,
`matrix upgrade-sim`) need a populated cache — see
[repos.md](repos.md).

## `repo`

Manage RPM repository metadata for repo-aware analysis. Full
documentation in [repos.md](repos.md).

```text
rpm-spec-tool repo sync   [--profile NAME | --all-profiles | --target-set ID] --allow-fetch
rpm-spec-tool repo show   [--profile NAME] [--repo ID] [--package N | --provides N | --provides-like P | --file PATH] [--full]
rpm-spec-tool repo status [--profile NAME] [--format human|json]
rpm-spec-tool repo cache  ls | gc [--keep N] | prune [--repo ID] [--yes]
```

Network policy is a 3-state ladder — `Offline` (default), `CacheOnly`
(strict CI), `Online` (`--allow-fetch`). The environment variables
`RPM_SPEC_TOOL_OFFLINE=1`, `RPM_SPEC_TOOL_CACHE_ONLY=1`, and
`RPM_SPEC_TOOL_CACHE_DIR=/path` override the defaults.

The `--insecure-tls` flag disables certificate verification entirely.
**Do not use it in production CI** — fix the trust store via
`update-ca-trust` / `update-ca-certificates` instead.

## `lints`

Print the catalogue of every built-in lint rule.

```text
rpm-spec-tool lints
    [--format text|markdown]
    [--category style|correctness|packaging|performance]...
    [--severity allow|warn|deny]...
```

* `--category` and `--severity` are repeatable; values inside one flag
  OR-combine, distinct flags AND-combine.
* `--format markdown` is the source of truth for
  [`doc/lints-list.md`](lints-list.md), which CI's `lints-doc-check`
  job keeps in sync.

Use this command when you want to know what the analyzer can *do*
right now — `lints --severity allow` answers "what's silenced by
default?".

## `config`

Manage `rpmspec.toml` — generate it, validate it, and emit
documentation / JSON-Schema for it.

```text
rpm-spec-tool config init     [--output PATH | --stdout | --dry-run]
                              [--profile NAME] [--all-lints] [--force] [--yes]
rpm-spec-tool config validate [PATH]
rpm-spec-tool config schema   [--format json|jsoncompact]
rpm-spec-tool config doc      [--field NAME]
```

* `init` defaults to the XDG config path
  (`~/.config/rpm-spec-tool/rpmspec.toml`). Use `--output PATH` to put
  it next to your specs instead.
* `--all-lints` embeds every built-in rule as a commented severity
  entry, turning the file into a discoverable catalogue.
* `validate` walks upward from the current directory when no path is
  given, mirroring discovery.
* `schema --format json` writes the JSON-Schema you'd point a TOML
  editor (taplo / VS Code / Helix / Zed) at for completion.
* `doc --field <NAME>` prints the markdown reference page for one
  section (`lints`, `format`, `shellcheck`, `profiles`, `targets`).

See [configuration.md](configuration.md) for the full schema and
discovery rules.

## `completions`

Print a shell completion script to stdout.

```text
rpm-spec-tool completions <SHELL>
```

Supported shells: `bash`, `zsh`, `fish`, `powershell`, `elvish`.
Installation examples — same ones the command's `--help` prints:

```bash
rpm-spec-tool completions bash       | sudo tee /etc/bash_completion.d/rpm-spec-tool >/dev/null
rpm-spec-tool completions zsh        > ~/.zsh/completions/_rpm-spec-tool
rpm-spec-tool completions fish       > ~/.config/fish/completions/rpm-spec-tool.fish
rpm-spec-tool completions powershell >> $PROFILE
```
