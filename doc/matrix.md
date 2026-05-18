# Release target sets and the matrix runner

`matrix check` runs the analyzer against a *release matrix* — a set of
distribution profiles you expect the same `.spec` to build under — and
aggregates findings so cross-platform regressions show up as one
record per root cause rather than one per profile.

If you are new to profiles, read [`profiles.md`](profiles.md) first.
This document covers the multi-profile layer that sits on top.

## TL;DR

```toml
# .rpmspec.toml
[targets.product-2026q2]
profiles = [
  "rhel-8-x86_64",
  "rhel-9-x86_64",
  "altlinux-10-x86_64",
  "altlinux-10-e2k",
  "sles-15-x86_64",
]
defines = { product_build = "1" }

[targets.product-2026q2.profile-overrides."altlinux-10-e2k"]
defines = { use_jit = "0" }
```

```bash
rpm-spec-tool target list
rpm-spec-tool target show product-2026q2
rpm-spec-tool matrix check --target-set product-2026q2 product.spec
```

## `[targets.<name>]` schema

| Field                          | Type                                  | Notes                                                                                     |
|--------------------------------|---------------------------------------|-------------------------------------------------------------------------------------------|
| `profiles`                     | `array<string>`                       | Ordered list of profile names — built-in or from `[profiles.*]`. Order drives column order. |
| `defines`                      | `table<string, string>`               | `--define`-style overrides applied to every member profile.                                |
| `profile-overrides.<profile>`  | `table` keyed by a profile from above | Per-profile outlier overrides; only `defines` for now.                                     |

### Resolution rules

* `profiles` must be non-empty.
* Each key in `profile-overrides` must appear in `profiles` (typos fail
  the resolve rather than silently no-oping).
* Duplicate entries in `profiles` are collapsed to first occurrence so
  the matrix table doesn't show duplicated columns.

### Defines precedence

For one resolved member of a target set, defines stack from low to high
precedence:

1. The profile's own `[profiles.X.macros]` and bundled showrc.
2. `targets.<name>.defines` (applied to every member).
3. `targets.<name>.profile-overrides.<profile>.defines` (per-profile).
4. CLI `--define NAME VALUE` passed to `matrix check`.

Last write wins for any given macro name. Phase 1 records steps 2–4
under a single `LayerInfo::CliDefine` layer; per-source attribution
at the layer level is a follow-up.

## CLI

### `target list`

Tabular catalogue of every `[targets.<name>]` declared in the loaded
config. Empty config prints a hint line — the matrix mode is opt-in.

### `target show <NAME>`

Resolves one target set and pretty-prints its member profiles with
family / vendor / dist-tag, macro counts, and any per-profile overrides.

### `matrix check`

```text
rpm-spec-tool matrix check [SPECS...]
    (--target-set NAME | --profiles A,B,C)
    [--define 'NAME VALUE']...
    [--deny LINT]... [--warn LINT]... [--allow LINT]...
    [--format human|json|sarif]
```

Either `--target-set` or `--profiles` is required (and they are
mutually exclusive). `--profiles` synthesises an ad-hoc target set
labelled `<ad-hoc>` in the output — useful for one-off checks without
touching the config.

#### Exit codes

* `0` — success (no `deny`-severity finding on any profile).
* `1` — at least one `deny`-severity finding on at least one profile.
* `2` — user error (unknown target name, malformed `--define`,
  missing required flag).

## Output formats

### Human (default)

Per-spec table (one row per profile) followed by aggregated findings
grouped by signature:

```text
# Matrix run: target set `product-2026q2`
  profiles: 5

## product.spec
  PROFILE                        DENY   WARN  PARSE
  rhel-8-x86_64                     0      2     OK
  rhel-9-x86_64                     0      0     OK
  altlinux-10-x86_64                0      1     OK
  altlinux-10-e2k                   1      0     OK
  sles-15-x86_64                    0      3     OK

  [deny] RPM404 missing-buildrequires (1/5)
    `meson` used in %build without BuildRequires
    line 42, column 1
    affected: altlinux-10-e2k
  [warn] RPM055 summary-ends-with-dot (5/5)
    Summary ends with a `.`
    line 4, column 1
    affected: altlinux-10-e2k, altlinux-10-x86_64, rhel-8-x86_64, rhel-9-x86_64, sles-15-x86_64
```

### JSON (`--format json`)

```json
{
  "target_set": "product-2026q2",
  "profiles": ["rhel-8-x86_64", "rhel-9-x86_64", "..."],
  "files": [
    {
      "path": "product.spec",
      "per_profile": [
        {
          "profile": "rhel-8-x86_64",
          "parse_ok": true,
          "diagnostics": [ /* per-profile Diagnostic shape */ ]
        }
      ],
      "aggregated": [
        {
          "matrix_signature": "<16 hex chars>",
          "lint_id": "RPM404",
          "lint_name": "missing-buildrequires",
          "severity": "deny",
          "message": "...",
          "primary_span": { "start_byte": ..., "start_line": ..., "..." },
          "affected_profiles": ["altlinux-10-e2k"]
        }
      ]
    }
  ]
}
```

`MatrixJsonReport` is distinct from the `lint` JSON shape — tooling
should parse them as separate formats.

### SARIF (`--format sarif`)

SARIF 2.1.0 envelope. Each `Result` is a per-profile finding with
matrix metadata in `properties`:

```json
"properties": {
  "profile": "rhel-9-x86_64",
  "target_set": "product-2026q2",
  "matrix_signature": "...",
  "affected_profiles": ["rhel-8-x86_64", "rhel-9-x86_64"]
}
```

SARIF tolerates arbitrary properties, so consumers that ignore
`properties` still read the result list correctly.

## Aggregation contract

Two findings are merged into one aggregated entry iff their
`(lint_id, primary_span byte range, message)` triple is identical.

* Findings on different lines, or with different lint IDs, stay
  separate.
* Findings with profile-dependent text (a macro value embedded in the
  message) currently split into N aggregated entries with overlapping
  `affected_profiles`. Rules that want cross-profile aggregation
  should produce profile-stable messages.

The Phase 2 plan is to normalise messages by stripping known
profile-specific tokens (macro values, arch names) before hashing —
that will collapse the split-by-message case.

## Baseline mode

For legacy specs with a large existing warning corpus, baseline mode
suppresses already-known findings so CI can gate on *new* regressions
only.

### Create a baseline

```bash
rpm-spec-tool matrix baseline create \
  --target-set product-2026q2 product.spec --out baseline.json
```

The baseline is a small versioned JSON document. Commit it to the
repo; PRs that introduce new findings will be visible in the diff.

```json
{
  "baseline_version": 1,
  "entries": [
    {
      "matrix_signature": "...",
      "lint_id": "RPM055",
      "message": "Summary ends with a `.`",
      "affected_profile_count": 5
    }
  ]
}
```

`affected_profile_count` is informational. If it grows in a
re-recorded baseline without the `matrix_signature` changing, the
same finding now affects more profiles than before — the file gives
you a place to spot that in a PR diff.

### Use it in CI

```bash
rpm-spec-tool matrix check \
  --target-set product-2026q2 \
  --baseline baseline.json \
  --fail-on new product.spec
```

* `--fail-on all` (default) — any deny finding fails, same as
  Phase 1 semantics.
* `--fail-on new` — only findings whose `matrix_signature` is NOT
  in the baseline contribute to a non-zero exit. Requires
  `--baseline`; without it the command exits 2 ("`--fail-on new
  requires --baseline FILE`") so a misconfigured CI step cannot
  silently degrade to "every finding is new".

In `human` output every matched entry is tagged `[baseline]`; the
JSON / SARIF views are unchanged so existing tooling keeps working.
Programmatic consumers that want to filter new vs. existing should
load the baseline themselves and join on `matrix_signature`.

### Baseline file validation

The reader rejects malformed input up-front rather than letting it
through as "no matches":

* Unsupported `baseline_version` → exit 2 with `regenerate or
  upgrade rpm-spec-tool` hint.
* Any `matrix_signature` not matching the 16 lowercase hex contract
  of [`MatrixSignature::Display`] → exit 2, with the offending
  entry index and `lint_id` in the message.
* Unknown JSON fields → rejected (typos in the file can't silently
  become no-ops).
* File size capped at 16 MiB — defends shared CI runners from an
  oversized baseline consuming memory.

### Stability caveat

`matrix_signature` is built from `(lint_id, span, message)` via
`std::collections::hash_map::DefaultHasher`. The hash is stable within
one binary release but the stdlib does not contractually guarantee
cross-toolchain stability. In practice signatures rarely change, but
expect a one-time baseline refresh after a major rustc upgrade.

## Limitations of Phase 1

* No `matrix explain` / `coverage` / `portability` / `diff` /
  `impact` yet — those land in Phase 2.
* No parallel execution — profiles are analysed sequentially. Cost
  is linear in profile count; a profile set of 30 typically
  completes in a few seconds for one spec.
* Per-profile-override and target-wide defines share one
  `CliDefine` layer in the resolved profile, so `profile show` of
  a matrix-resolved profile can't yet distinguish the two sources
  from CLI `--define`.
