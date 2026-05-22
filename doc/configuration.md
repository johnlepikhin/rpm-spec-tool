# Configuration (`rpmspec.toml`)

`rpm-spec-tool` is fully usable without any configuration — every
subcommand has reasonable defaults and 24 distribution profiles ship
in the binary. A config file enters the picture when you want to:

* fix the active distribution profile so collaborators see the same
  diagnostics,
* override lint severities,
* declare a multi-profile release matrix (`[targets.*]`),
* attach RPM repositories to profiles for repo-aware checks
  (`[profiles.X.repos.*]`),
* declare allowed value sets for build-time macros
  (`[macros.*]`).

This page covers the schema and discovery rules. For the surface area
of the four `config` subcommands see [§ The `config` subcommand
tree](#the-config-subcommand-tree) below.

## Discovery

The tool resolves `rpmspec.toml` via a fixed cascade — no walk-up,
so the same spec lints the same way regardless of CWD:

1. `--config PATH` on the command line. Explicit wins over everything.
2. `$RPM_SPEC_TOOL_CONFIG` if set.
3. `$XDG_CONFIG_HOME/rpm-spec-tool/rpmspec.toml`
   (typically `~/.config/rpm-spec-tool/rpmspec.toml`).
4. Built-in defaults if none of the above exist.

`config validate` is the one exception — when called with no path
and the cascade above finds nothing, it walks upward from CWD
looking for `.rpmspec.toml` as a last fallback (see
[§ The `config` subcommand tree](#the-config-subcommand-tree) below).

Paths inside the file (`showrc-file`, etc.) are resolved relative to
the directory the config was found in, **not** the CWD — so multiple
checkouts of the same packaging repo behave consistently regardless of
where you run the tool from.

## Generating a starter file

```bash
# Drop a heavily-commented starter at the XDG location:
rpm-spec-tool config init --profile rhel-9-x86_64

# Or next to your specs:
rpm-spec-tool config init --output ./rpmspec.toml --profile rhel-9-x86_64

# Bake every built-in lint as a commented entry — the file doubles
# as a discoverable per-rule catalogue:
rpm-spec-tool config init --all-lints --output ./rpmspec.toml
```

Validate an existing file (uses the discovery cascade, with an upward walk from CWD as a final fallback unique to `validate`):

```bash
rpm-spec-tool config validate
```

## Minimal example

```toml
# Pick the active distribution profile. CLI `--profile` overrides this.
profile = "rhel-9-x86_64"

[lints]
# Per-lint severity overrides. CLI `--deny/--warn/--allow` win.
RPM031              = "warn"
missing-changelog   = "deny"
mixed-spaces-and-tabs = "allow"

[shellcheck]
# Tune the optional shellcheck integration (RPM200).
binary       = "shellcheck"
timeout_secs = 10
```

Rule IDs (`RPM031`) and names (`missing-changelog`) are
interchangeable in `[lints]`. The canonical list lives in
[lints-list.md](lints-list.md).

## Top-level sections

The full schema is also available as JSON Schema (`config schema`)
and as auto-generated markdown (`config doc`). The summary below
covers each section's purpose; lean on the generators for an
always-current field reference.

### `profile = "<name>"`

The default profile. May reference a built-in (see
[profiles.md](profiles.md#built-in-distribution-profiles)) or a key
from `[profiles.*]`. CLI `--profile` overrides it.

### `[lints]`

Per-rule severity table. Keys are rule IDs (`RPM031`) **or**
short names (`missing-changelog`); values are `"allow"`, `"warn"`, or
`"deny"`. See [lints.md](lints.md) for the severity model.

### `[format]`

Tunes `format` and `pretty`.

```toml
[format]
preamble-align-column = 16   # column for `Tag: value` alignment
conditional-indent    = 0    # spaces per nested %if level (cosmetic only, > 0 forbidden for commits)
```

### `[shellcheck]`

Controls the optional `shellcheck` integration (`RPM200`).

```toml
[shellcheck]
binary       = "shellcheck"  # path or binary name
timeout_secs = 10
dialect      = "bash"         # sh | bash | dash | ksh
disable      = ["SC2086"]
enable       = ["SC2164"]
```

Set the top-level `shellcheck = "allow"` (via `[lints]`) to disable
the integration entirely.

### `[profiles.<name>]`

Define a user-level distribution profile or override fields of a
built-in. The full layering rules — `extends`, `showrc-file`,
auto-detect of identity from `rpm --showrc`, etc. — live in
[profiles.md](profiles.md). Minimal example:

```toml
[profiles.acme-rhel-9]
extends    = "rhel-9-x86_64"

[profiles.acme-rhel-9.identity]
vendor   = "acme"
dist-tag = ".acme1"

[profiles.acme-rhel-9.macros]
_vendor = "acme"
```

### `[profiles.<name>.repos.<id>]`

Attach RPM repositories to a profile. Full schema and the offline
policy in [repos.md](repos.md).

```toml
[profiles.acme-rhel-9.repos.baseos]
baseurl  = "https://internal-mirror.example/rhel/9/BaseOS/$basearch/os/"
kind     = "rpm-md"
priority = 10
role     = "base"
```

### `[targets.<name>]`

Group multiple profiles into a release matrix. Used by every `matrix`
subcommand and by `target list` / `target show`. Detailed in
[matrix.md](matrix.md#targetsname-schema).

```toml
[targets.release-2026]
profiles = ["rhel-9-x86_64", "altlinux-10-x86_64", "sles-15-x86_64"]
defines  = { product_build = "1" }

[targets.release-2026.profile-overrides."altlinux-10-x86_64"]
defines = { use_jit = "0" }
```

### `[macros.<name>]`

Declare the allowed value set of a build-time macro (`-D` flag) so
`matrix coverage` can tag a branch `[CONDITIONAL: macro=value]`
instead of `[DEAD]`. Lets coverage tell genuinely-dead code apart
from code that's just inactive under the current build's `-D`.

```toml
[macros.edition]
values      = ["community", "premium", "oem"]
description = "Build edition selector"

[macros.major_version]
values = ["13", "14", "15", "16", "17"]
```

Cartesian product is bounded at 64 combinations per branch; branches
that exceed the cap skip variant analysis with a `tracing::warn`. Full
semantics in [matrix.md § Macro variants](matrix.md#macro-variants).

## Override precedence

For severities, defines, and macros, lower → higher precedence (last
write wins):

1. Built-in defaults.
2. Bundled showrc for built-in profiles.
3. User `showrc-file` if specified.
4. `[profiles.<name>.*]` overrides in `rpmspec.toml`.
5. `[targets.<name>.defines]` (for matrix runs only).
6. `[targets.<name>.profile-overrides.<profile>.defines]`.
7. CLI `--define NAME VALUE` / `--deny LINT` / `--warn LINT` /
   `--allow LINT` / `--profile NAME`.

CLI always wins. The `profile show --full` subcommand surfaces the
resolved precedence for every macro via a per-entry `provenance` tag.

## The `config` subcommand tree

### `config init`

```text
rpm-spec-tool config init
    [--output PATH | --stdout | --dry-run]
    [--profile NAME] [--all-lints] [--force] [--yes]
```

* No flags → writes the XDG config path
  (`~/.config/rpm-spec-tool/rpmspec.toml`). Parents are created if
  missing.
* `--output PATH` writes to an explicit path. Refuses to overwrite an
  existing file unless `--force` (and `--yes` in non-interactive
  mode).
* `--stdout` prints to stdout instead of writing.
* `--dry-run` prints the rendered content and a `dry-run: would write
  to <path>` note on stderr without touching the filesystem.
* `--profile NAME` sets the `profile = …` key. Name is *not* validated
  against the built-in list — anything goes, useful when you plan to
  add it under `[profiles.*]` below.
* `--all-lints` emits every built-in lint as a commented `#
  lint-name = "severity"` line.

### `config validate`

```text
rpm-spec-tool config validate [PATH]
```

Parses the TOML and reports any deserialisation errors with file:line
spans. When `PATH` is omitted, applies the same cascade as the rest
of the tool (`--config`, `$RPM_SPEC_TOOL_CONFIG`, XDG default) and,
as a final fallback unique to `validate`, walks upward from CWD
looking for `.rpmspec.toml`.

### `config schema`

```text
rpm-spec-tool config schema [--format json|jsoncompact]
```

Emits the JSON Schema for `rpmspec.toml`. Pipe to a file and point your
TOML editor (taplo / VS Code / Helix / Zed) at it for inline
completion and validation:

```bash
rpm-spec-tool config schema > .rpmspec.schema.json
```

For taplo (`~/.config/taplo/taplo.toml`):

```toml
[[rule]]
include = ["**/rpmspec.toml", "**/.rpmspec.toml"]
schema  = { path = ".rpmspec.schema.json" }
```

VS Code with the
[Even Better TOML](https://marketplace.visualstudio.com/items?itemName=tamasfe.even-better-toml)
extension picks the schema up via the same path.

### `config doc`

```text
rpm-spec-tool config doc [--field NAME]
```

Render the full markdown reference for every config field — generated
from the same JSON Schema. With `--field NAME` (one of `lints`,
`format`, `shellcheck`, `profiles`, `targets`) it narrows to one
section. Useful for spot-checking what's available without leaving the
shell.

## Editor support summary

* TOML schema completion: `rpm-spec-tool config schema > .rpmspec.schema.json`.
* Spec file LSP: `rpm-spec-lsp` — diagnostics, code actions, hover,
  goto-definition, completion. See
  [editor-integration.md](editor-integration.md).
* CLI completions: `rpm-spec-tool completions <SHELL>` — see
  [cli.md § completions](cli.md#completions).
