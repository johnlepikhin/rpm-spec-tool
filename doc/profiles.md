# Distribution profiles

A *profile* describes the target build environment: distribution identity
(family / vendor / dist-tag), macros, rpmlib features, and license / group
whitelists. The analyzer does not guess anything about the host — it
reads only what the profile tells it.

Profiles are configured in `.rpmspec.toml`. Optionally, you can drop in a
dump of `rpm --showrc` taken on the target machine: the analyzer's parser
extracts identity and all macros from it automatically.

## Minimal config

```toml
profile = "rhel-9"

[profiles.rhel-9]
showrc-file = "vendor/rpm-showrc-rhel9.txt"
```

Generate the `vendor/rpm-showrc-rhel9.txt` dump on the target machine:

```bash
rpm --showrc > vendor/rpm-showrc-rhel9.txt
```

That is enough: identity (`family=RHEL`, `vendor=redhat`, `dist-tag=.el9`)
and all 700+ macros are extracted from the dump automatically.

## Active profile

The active profile is resolved in this order:

1. CLI flag `--profile <name>` (on the `lint` / `check` subcommands).
2. The `profile = "<name>"` key in `.rpmspec.toml`.
3. The built-in `generic` profile (empty template).

`<name>` may reference:

- a built-in profile (see the "Built-in distribution profiles" section
  below), or
- a key from a `[profiles.<name>]` section.

Several `[profiles.*]` sections may coexist in one config — useful when a
repository builds the same spec for multiple target distributions.
Exactly **one** profile is active per run; there is no automatic merging
between different profiles.

## Profile layers

When resolving the active profile, the analyzer layers data from lowest
to highest priority:

1. **Builtin baseline** — `data/<extends>.toml` (`generic` by default).
2. **Builtin showrc layer** — for built-in distribution profiles, a real
   `rpm --showrc` dump from the target machine is embedded in the binary
   (`data/<name>.showrc`). Applied immediately after the TOML metadata.
3. **Showrc layer** — the contents of the file referenced by
   `showrc-file`, if specified. Layered on top of the builtin showrc;
   the user dump wins on macro-name collisions.
4. **Auto-detect identity** — `vendor`, `dist-tag`, `family` are derived
   from showrc macros (only for fields the user did not set explicitly).
   Auto-detect runs across both showrc layers.
5. **Inline overrides** — `[profiles.<name>.*]` sections in the config.

The `rpm-spec-tool profile show [NAME]` subcommand prints the resolved
profile and the list of layers applied. With `--full` it dumps the
entire macro registry with provenance annotations.

## Auto-detect identity

The following fields are extracted from showrc:

| `Profile` field   | Source in showrc                                                                |
| ----------------- | ------------------------------------------------------------------------------- |
| `identity.vendor` | the `_vendor` macro (literal value)                                             |
| `identity.dist_tag` | the `dist` macro (literal value)                                              |
| `identity.family` | first marker found, in order: `altlinux`, `mageia`, `suse_version`, `rhel`, `fedora` |

The family resolution order is fixed. Derived distributions (AlmaLinux,
Rocky, CentOS Stream) expose several markers at once — the order
guarantees that they end up in their parent family. Any field the user
sets explicitly in `[profiles.X.identity]` wins over auto-detect.

## Full config

```toml
profile = "rhel-9-prod"

[profiles.rhel-9-prod]
extends = "generic"                          # base builtin; default = "generic"
showrc-file = "vendor/rpm-showrc-rhel9.txt"  # path relative to .rpmspec.toml

# All sections below are optional. Use them only if you need to
# override what showrc / the builtin layer already provides.

[profiles.rhel-9-prod.identity]
name = "RHEL 9 production"                   # human-readable label
family = "rhel"                              # fedora | rhel | opensuse | alt | mageia | generic
vendor = "mycompany"
dist-tag = ".mc1"

[profiles.rhel-9-prod.macros]
# Short form: literal value.
_vendor = "mycompany"
# Expanded form (for parameterised or multiline macros).
custom = { value = "...", opts = "(n:)" }

[profiles.rhel-9-prod.licenses]
mode = "strict"            # off (default) | warn | strict
replace = false            # false (default) = union with builtin/showrc; true = replace entirely
allow = ["GPL-2.0-or-later", "Proprietary"]

[profiles.rhel-9-prod.groups]
mode = "warn"
allow = ["System Environment/Daemons"]

# A second profile — for example, an ALT build target.
[profiles.altp10]
showrc-file = "vendor/rpm-showrc-altp10.txt"
```

## Merge semantics

- **Macros**: on a name collision the later layer wins; the entry's
  `provenance` is updated (visible via `profile show --full`).
- **Identity fields**: an explicit override in `[profiles.X.identity]`
  wins over auto-detect.
- **Licenses / groups**: with `replace = false` (default) lists are
  union-merged; with `replace = true` they are reset and reinitialised.
  The `mode` field is sticky — it only changes when a layer sets it
  explicitly. Default is `off` (the corresponding lints stay silent).

## `profile` subcommands

### `profile list`

Tabular catalogue of every available profile — built-ins plus those
defined in `.rpmspec.toml`. The active profile is marked with `*` in
the first column.

```bash
# Full list (builtin + user).
rpm-spec-tool profile list

# Built-ins only.
rpm-spec-tool profile list --builtin-only

# Only user-defined profiles from the loaded .rpmspec.toml.
rpm-spec-tool profile list --user-only
```

Columns for built-ins: `NAME · FAMILY · VENDOR · DIST-TAG · MACROS · ARCH`.
Columns for user-defined profiles: `NAME · EXTENDS · DETAILS` (DETAILS
summarises `showrc-file` and lists which sections were overridden —
`vendor`, `family`, `macros`, `licenses`, …).

### `profile show`

Details for a single profile — identity, layer chain, counts.

```bash
# Show the profile selected by config or CLI.
rpm-spec-tool profile show

# Resolve a specific profile by name.
rpm-spec-tool profile show rhel-9-x86_64

# Dump the full macro registry (with provenance per entry).
rpm-spec-tool profile show --full
```

Useful when debugging: with `--full` every macro is printed with a source
annotation (`showrc:-13`, `override`, `builtin:generic`).

### `profile macros`

List of a single profile's macro registry with filtering by name and/or
source. Unlike `show --full`, it does not print the identity block and
lets you narrow the output.

```bash
# All macros in the active profile.
rpm-spec-tool profile macros

# All macros in a specific profile.
rpm-spec-tool profile macros rhel-9-x86_64

# Filter by name substring (case-insensitive).
rpm-spec-tool profile macros rhel-9-x86_64 --filter optflags

# Filter by source: builtin / showrc / override.
rpm-spec-tool profile macros rhel-9-x86_64 --source override
```

Columns are aligned; long multiline values collapse to a
`<multiline N chars>` marker — use `profile macro` for the full body.

### `profile common`

Intersection of macro registries across two or more profiles — answers
"which macros are shared?". Two modes via `--mode`:

* **`--mode existence`** (default) — a macro is "common" when it is
  defined in every profile, regardless of value. Output is a simple
  name list.
* **`--mode value`** — adds the requirement that values match (`opts` +
  macro body). `Provenance` is ignored: a macro inherited from showrc in
  one profile and overridden in `.rpmspec.toml` in another counts as
  identical if the value matches. Output is `name = value`.

```bash
# No arguments: intersection across every built-in profile with a
# non-empty registry (i.e. excludes generic).
$ rpm-spec-tool profile common
# Common macros across 23 profile(s): 188

  ___build_args
  ___build_cmd
  ...

# An explicit set of profiles, existence mode.
$ rpm-spec-tool profile common rhel-8-x86_64 rhel-9-x86_64 rhel-10-x86_64
# Common macros across 3 profile(s): 292

  __7zip
  ___build_args
  ...

# Same set in value mode — the narrow slice of truly portable defaults.
$ rpm-spec-tool profile common --mode value rhel-8-x86_64 rhel-9-x86_64 rhel-10-x86_64
# Macros with identical values across 3 profile(s): 242

  __7zip          = /usr/bin/7za
  ___build_args   = -e
  ___build_shell  = %{?_buildshell:%{_buildshell}}%{!?_buildshell:/bin/sh}
  ...

# With a name filter.
$ rpm-spec-tool profile common --filter build rhel-8-x86_64 rhel-9-x86_64
# Common macros across 2 profile(s): 358 total, 45 matching "build"
  ...
```

In `--mode value`, long values are truncated to 80 characters and
multiline bodies collapse to `<multiline N chars>`. At least two
profiles are required — a single argument is rejected with exit code
`2`. An empty intersection exits `0` with the `(no common macros)`
marker.

When `--filter` is active the header reports both counts:
`# Common macros across N profile(s): {total} total, {matching} matching "X"`
— `total` is the size of the full intersection, `matching` is after
applying the filter.

### `profile macro`

A general-purpose lookup of a single macro's value. Behaviour scales
with the number of profile arguments:

| Arguments              | Behaviour                                                                  | Exit code                |
| ---------------------- | -------------------------------------------------------------------------- | ------------------------ |
| `<macro>`              | Table of the value across **every available** profile                      | `0`                      |
| `<macro> <p>`          | Compact value in a single profile; multiline body is expanded line-by-line | `0` / `2` if undefined   |
| `<macro> <p1> <p2>…`   | Comparison table across the listed profiles                                | `0`                      |

```bash
# Compare a macro across every profile (24 builtin + user-defined).
$ rpm-spec-tool profile macro dist
# Macro `dist` across 24 profile(s)

  generic                 = (undefined)
  rhel-8-x86_64           = .el8                                [showrc:-13]
  rhel-9-x86_64           = %{!?distprefix0:...}.el9%{?distsuf… [showrc:-13]
  altlinux-10-x86_64      = (undefined)
  ...

# A specific profile — compact, with multiline bodies expanded.
$ rpm-spec-tool profile macro dist rhel-9-x86_64
dist = %{!?distprefix0:...}.el9...  [showrc:-13]

$ rpm-spec-tool profile macro ___build_pre rhel-9-x86_64
___build_pre =  [showrc:-13]
    RPM_SOURCE_DIR="%{u2p:%{_sourcedir}}"
    RPM_BUILD_DIR="%{u2p:%{_builddir}}"
    ...

# An arbitrary set of profiles to compare.
$ rpm-spec-tool profile macro dist rhel-8-x86_64 rhel-9-x86_64 altlinux-10-x86_64
# Macro `dist` across 3 profile(s)

  rhel-8-x86_64      = .el8                                  [showrc:-13]
  rhel-9-x86_64      = %{!?distprefix0:...}.el9...           [showrc:-13]
  altlinux-10-x86_64 = (undefined)
```

In the table modes long values are truncated to 80 characters; the full
body is available via the single-profile form. Exit code `2` is emitted
**only** in the single-profile mode (macro undefined), which is
convenient for shell:

```bash
if ! rpm-spec-tool profile macro __python3 sles-15-x86_64 >/dev/null; then
    echo "macro missing — fall back"
fi
```

## Built-in distribution profiles

A set of predefined profiles for common target systems is embedded in
the binary and works without a `.rpmspec.toml`:

```bash
rpm-spec-tool check --profile rhel-9-x86_64 build.spec
rpm-spec-tool profile show altlinux-10-e2k
```

Every distribution profile includes an `rpm --showrc` dump taken on a
live machine: the real macro registry (400–600 macros), rpmlib features,
arch / build-os, and identity (`family` / `vendor` / `dist-tag`).
Identity for distributions that expose marker macros is computed
automatically; for the rest (REDos, ALT Linux, Rosa, MOSos) `family` is
pinned in `data/<name>.toml`.

| Family        | Profiles                                                                                                                                |
| ------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| RHEL          | `rhel-8-x86_64`, `rhel-8-aarch64`, `rhel-9-x86_64`, `rhel-9-aarch64`, `rhel-10-x86_64`, `rhel-10-aarch64`                                |
| REDos         | `redos-7.3-x86_64`, `redos-7.3-aarch64`, `redos-8-x86_64`, `redos-8-aarch64`                                                             |
| ALT Linux     | `altlinux-10-x86_64`, `altlinux-10-aarch64`, `altlinux-10-e2k`, `altlinux-10-e2kv4`, `altlinux-11-x86_64`, `altlinux-11-aarch64`         |
| ALT Linux SPT | `altlinux-spt-10-x86_64`, `altlinux-spt-10-aarch64`, `altlinux-spt-10-e2k`, `altlinux-spt-10-e2kv4`                                      |
| openSUSE-like | `sles-15-x86_64`, `mosos-15-x86_64`                                                                                                     |
| Rosa          | `rosa-2021.1-x86_64`                                                                                                                    |
| baseline      | `generic` (empty template, default fallback)                                                                                            |

Any built-in profile can be used as a base for your own overrides:

```toml
profile = "ourbuild"

[profiles.ourbuild]
extends = "rhel-9-x86_64"

[profiles.ourbuild.identity]
vendor = "mycompany"
dist-tag = ".mc1"

[profiles.ourbuild.macros]
_vendor = "mycompany"
```

## Multi-platform matrix

A *target set* groups multiple profiles so the same spec can be checked
across an entire release matrix in one invocation, with findings
aggregated by affected profiles. See [`matrix.md`](matrix.md) for the
`[targets.<name>]` schema and the `matrix check` command.
