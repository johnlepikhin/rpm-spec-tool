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
  enforced by `MatrixSignature::from_hex` (the strict inverse of the
  `Display` form) → exit 2, with the offending entry index and
  `lint_id` in the message.
* Unknown JSON fields → rejected (typos in the file can't silently
  become no-ops).
* File size capped at 16 MiB — defends shared CI runners from an
  oversized baseline consuming memory.

### Baseline write atomicity

`matrix baseline create --out PATH` writes through a sibling temporary
file in `PATH`'s directory and renames into place on success. On POSIX
the rename is atomic, so a SIGINT or out-of-disk mid-write cannot
leave a partially-serialised baseline on disk to confuse the next
`matrix check --baseline` run. Stdout output (the default destination)
is not atomic — pipe through `> tmp && mv tmp baseline.json` if your
CI needs the same guarantee for stdout-redirected writes.

### Stability caveat

`matrix_signature` is built from `(lint_id, span, message)` via
`std::collections::hash_map::DefaultHasher`. The hash is stable within
one binary release but the stdlib does not contractually guarantee
cross-toolchain stability. In practice signatures rarely change, but
expect a one-time baseline refresh after a major rustc upgrade.

## Macro portability

`matrix portability` answers the question "which macros referenced by
my spec aren't defined on every target profile?". It walks the AST,
records every user-referenced macro name (skipping positional, flag,
and builtin language constructs), and looks each up in every member
profile's macro registry.

```bash
rpm-spec-tool matrix portability \
  --target-set product-2026q2 product.spec
```

Output (human):

```text
# Matrix portability: target set `product-2026q2` (5 profiles)

## product.spec
  73 macros referenced — 1 missing, 6 partial, 66 portable

  STATUS     MACRO                           DEF/TOTAL  MISSING ON
  missing    cmake_build                       0/5      generic, rhel-8-x86_64, ...
  partial    systemd_requires                  3/5      altlinux-10-x86_64, sles-15-x86_64
  partial    _libdir                           5/5      -
  portable   _bindir                           5/5      -
```

Statuses:

* `missing` — no profile defines the macro. Either the spec relies
  on a `--define` the user must supply, or it's a typo / removed
  macro.
* `partial` — some profiles define it, others don't. The most
  actionable category: usually means the spec needs a `%{?guard}`
  or a compatibility shim macro.
* `portable` — every profile defines it. Phase 2 may refine this
  to flag profiles where values *differ* (`_libdir` resolves to
  `/usr/lib64` vs `/usr/lib`).

### Exit codes

* `--fail-on none` (default) — informational, always exits 0.
* `--fail-on missing` — exits 1 if any macro is `missing`.
* `--fail-on partial` — exits 1 if any macro is `missing` *or*
  `partial`. Strictest mode for release engineering CI.

### JSON output

```bash
rpm-spec-tool matrix portability --format json --target-set X product.spec
```

emits `{ target_set, profiles, files[{ path, total_used, missing,
partial, portable, entries[{ name, status, defined_in, missing_in
}] }] }` — same shape as the human view, machine-readable for
dashboards.

## Branch coverage

`matrix coverage` evaluates every `%if` / `%ifarch` / `%ifos`
branch in the spec against every member profile and reports which
profiles activate it. Built on top of the same per-profile macro
registry the linter uses, so the evaluation is consistent with
what the rest of the tool sees.

```bash
rpm-spec-tool matrix coverage --target-set product-2026q2 product.spec
```

Output (human):

```text
# Matrix coverage: target set `product-2026q2` (5 profiles)

## product.spec
  4 branches — 1 dead, 1 indeterminate

  line 9: %if 0%{?rhel}
    active: rhel-8-x86_64, rhel-9-x86_64
    inactive: altlinux-10-x86_64, sles-15-x86_64
  line 13: %if 0 [DEAD]
    active: (none)
    inactive: altlinux-10-x86_64, rhel-8-x86_64, rhel-9-x86_64, sles-15-x86_64
  line 17: %ifarch e2k
    active: altlinux-10-e2k
    inactive: altlinux-10-x86_64, rhel-8-x86_64, rhel-9-x86_64, sles-15-x86_64
```

Tags:

* `[DEAD]` — branch is inactive on every profile in the set and no
  evaluation was indeterminate. The whole `%if…%endif` block is
  dead code and can be deleted.
* `[ALWAYS]` — branch is active on every profile. The condition has
  no effect; the body can be inlined.

Evaluator scope (Phase 2):

* `%ifarch` / `%ifnarch` / `%ifos` / `%ifnos` — strict equality
  against the profile's `build_arch` / `build_os`.
* `%if EXPR` — best-effort numeric evaluation. Macros are expanded
  via the profile's registry; `%{?foo}` patterns contribute the
  macro value when defined, empty when not. The result is parsed
  as `i64` (non-zero ⇒ active). When any sub-expression can't be
  resolved (undefined unconditional macro, opaque arithmetic,
  string comparison without literal sides) the branch is reported
  as `indeterminate` — surfacing it for human review rather than
  guessing.

### Exit codes

* `--fail-on none` (default) — informational, always exits 0.
* `--fail-on dead` — exits 1 if any branch is dead across the set.
* `--fail-on indeterminate` — exits 1 if any branch is dead *or*
  indeterminate. Strict mode for CI gating where every branch must
  be evaluated.

### JSON output

```bash
rpm-spec-tool matrix coverage --format json --target-set X product.spec
```

emits `{ target_set, profiles, files[{ path, total_branches,
dead_branches, indeterminate_branches, conditionals[{ start_line,
has_else, branches[{ line, display, is_dead, is_universally_active,
active_on, inactive_on, indeterminate_on, indeterminate_reasons }] }] }] }`
— same data as the human view, indexed by line for dashboards.

`is_universally_active` is `true` when the branch evaluates to active
on every profile in the set — the condition has no effect and the
body is a candidate for inlining. `indeterminate_reasons` is a map
from profile id to a human-readable explanation of why evaluation
could not produce a definite verdict; it is populated only for
profiles listed in `indeterminate_on`.

## Explain mode

`matrix explain` answers the focused question "why is this line / macro
behaving differently across profiles?" without dumping the full
`matrix check` report. Two mutually-exclusive query modes:

```bash
rpm-spec-tool matrix explain product.spec \
  --target-set product-2026q2 --line 183

rpm-spec-tool matrix explain product.spec \
  --target-set product-2026q2 --macro _unitdir
```

Exactly one of `--line N` and `--macro NAME` is required (clap
rejects both-or-neither with exit 2), and exactly one of
`--target-set` / `--profiles` selects the matrix. Only one spec file
is accepted per invocation — explain is a focused tool, not a batch
reporter.

### `--line N` semantics

`--line N` reports every enclosing `%if`/`%ifarch`/`%ifos` branch
whose conditional span (from the directive to its matching `%endif`)
covers line `N`. For each such branch the output shows the active /
inactive / indeterminate profile lists, mirroring the per-branch
section of `matrix coverage`. Nested chains surface as multiple
entries, outer-first, so the reader sees the full decision path.

Human output:

```text
# Matrix explain: target set `product-2026q2` (10 profiles)
## product.spec
  line 183
    branch 182: %if 0%{?rhel}
      active:   rhel-8-x86_64, rhel-9-x86_64, rhel-9-aarch64
      inactive: altlinux-10-x86_64, sles-15-x86_64
```

When no branch covers the line (typical for preamble lines or lines
in `%description`/`%files` outside any condition), the output is one
line: `(no enclosing %if/%ifarch/%ifos branch covers this line)`.

### `--macro NAME` semantics

`--macro NAME` reports the literal-expanded value of `NAME` on every
member profile, plus `(undefined)` for profiles that don't register
the macro. Macros whose body cannot be reduced to a literal at lint
time (e.g. shell expansions, parameterised bodies) render as
`(defined but body not literal-expandable)`.

The depth budget for expansion is 8 levels — deep enough for nested
path helpers like `%_libdir = %{_prefix}/lib64 → %_prefix = /usr` but
bounded against cyclic registrations.

### JSON envelope

Both modes use `--format json` to emit a tagged envelope:

```json
{
  "query": "line",
  "target_set": "product-2026q2",
  "profiles": ["rhel-9-x86_64", "altlinux-10-x86_64"],
  "path": "product.spec",
  "line": 183,
  "branches": [
    {
      "branch_line": 182,
      "display": "%if 0%{?rhel}",
      "is_dead": false,
      "is_universally_active": false,
      "active_on": ["rhel-9-x86_64"],
      "inactive_on": ["altlinux-10-x86_64"],
      "indeterminate_on": [],
      "indeterminate_reasons": {}
    }
  ]
}
```

```json
{
  "query": "macro",
  "target_set": "product-2026q2",
  "profiles": ["rhel-9-x86_64", "altlinux-10-x86_64"],
  "path": "product.spec",
  "name": "_unitdir",
  "entries": [
    {
      "profile_id": "rhel-9-x86_64",
      "defined": true,
      "value": "/usr/lib/systemd/system",
      "unexpandable_reason": null
    },
    {
      "profile_id": "altlinux-10-x86_64",
      "defined": false,
      "value": null,
      "unexpandable_reason": null
    }
  ]
}
```

`unexpandable_reason` is non-null only when `defined: true` and
`value: null` — i.e. the macro is registered but its body can't be
reduced to a literal at lint time.

## Expand mode

`matrix expand` prints the spec source per profile with each branch
directive line (`%if` / `%elif` / `%else` / `%ifarch` / `%ifnarch` /
`%ifos` / `%ifnos`) tagged `[ACTIVE]` / `[INACTIVE]` /
`[INDETERMINATE: <reason>]` according to the branch evaluator's
verdict on that profile. It is the static
analogue of `rpmspec -P`: macros are NOT expanded and inactive
branch bodies stay in place so the reader can scan past them.

```bash
rpm-spec-tool matrix expand product.spec \
  --target-set product-2026q2
```

`matrix expand` is single-spec only — multi-spec batches across a
target set with 10+ profiles drown the signal. Reject explicitly
with exit 2.

### Human output

```text
# Matrix expand: target set `product-2026q2` (2 profiles)
## product.spec

== Profile rhel-9-x86_64 ==
Name:    foo
Version: 1.0
…
%if 0%{?rhel}  [ACTIVE]
BuildRequires: rhel-pkg
%endif

== Profile altlinux-10-x86_64 ==
Name:    foo
…
%if 0%{?rhel}  [INACTIVE]
BuildRequires: rhel-pkg
%endif
```

Each profile section is prefixed with `== Profile <id> ==`. Lines
that are not branch directives render verbatim. Indeterminate
branches carry the evaluator's reason inline:

```text
%if 0%{?rhel} >= 8  [INDETERMINATE: unsupported condition shape: arithmetic in Raw condition requires Parsed CondExpr]
```

### JSON output

```json
{
  "target_set": "product-2026q2",
  "profiles": ["rhel-9-x86_64", "altlinux-10-x86_64"],
  "path": "product.spec",
  "per_profile": [
    {
      "profile_id": "rhel-9-x86_64",
      "branches": [
        {
          "line": 8,
          "directive": "%if 0%{?rhel}",
          "status": "active",
          "indeterminate_reason": null
        }
      ]
    }
  ]
}
```

The full source text is intentionally NOT serialised — tooling
consumers can re-read the spec themselves and join on `line`. The
`status` discriminator uses `snake_case`: `active` / `inactive` /
`indeterminate`. `indeterminate_reason` is non-null only when
`status == "indeterminate"`.

### Exit codes

* `0` — always (`expand` is informational, never gates).
* `2` — usage error: missing `--target-set`/`--profiles`,
  unresolvable target set, multiple input specs, etc.

## Contract verification

`matrix verify-contract` gates the spec against per-profile
expectations declared in a separate TOML contract document. Phase 7
ships `must_have_buildrequires` / `must_not_have_buildrequires` only;
future phases can add `binary_packages` / `files` / `arch` assertions
additively (`#[serde(default)]` on every new field keeps older
contracts forward-compatible).

```bash
rpm-spec-tool matrix verify-contract product.spec \
  --target-set product-2026q2 \
  --contract product-contract.toml
```

`--contract PATH` is required — without explicit expectations the
command would silently pass and a misconfigured CI step couldn't be
distinguished from a healthy run.

### Contract schema

```toml
[profiles."rhel-9-x86_64"]
must_have_buildrequires = ["gcc", "make"]
must_not_have_buildrequires = ["egrep"]

[profiles."altlinux-10-x86_64"]
must_have_buildrequires = ["rpm-build"]
```

Keys under `[profiles."..."]` are profile identifiers, matched
case-sensitively against the resolved `--target-set` / `--profiles`
member list. Profiles absent from the contract are silently skipped
during verification — they surface as a `(no contract — skipping)`
row so operators can spot the missing block.

Unknown top-level or per-profile fields are rejected
(`#[serde(deny_unknown_fields)]`) so a typo can't be mistaken for a
no-op.

### Output

Human output:

```text
# Matrix verify-contract: target set `product-2026q2` (3 profiles)

## product.spec
  rhel-9-x86_64: FAIL (1 violation(s))
    [missing] missing-pkg
  altlinux-10-x86_64: PASS
  sles-15-x86_64: (no contract — skipping)
```

JSON output reuses the analyzer's `ContractReport` shape verbatim:

```json
{
  "target_set": "product-2026q2",
  "profiles": ["rhel-9-x86_64", "altlinux-10-x86_64"],
  "files": [
    {
      "path": "product.spec",
      "per_profile": [
        {
          "profile_id": "rhel-9-x86_64",
          "status": {
            "kind": "violations",
            "violations": [
              {"kind": "missing_required", "package": "missing-pkg"},
              {"kind": "forbidden_present", "package": "egrep", "found_as": "egrep"}
            ]
          }
        },
        {
          "profile_id": "altlinux-10-x86_64",
          "status": {"kind": "pass"}
        }
      ]
    }
  ]
}
```

The `status.kind` discriminator is one of `pass` / `no_contract` /
`violations`. Each violation carries its own `kind` discriminator
(`missing_required` or `forbidden_present`). All discriminators use
`snake_case`.

### Exit codes

* `0` — every profile either passes or has no contract.
* `1` — at least one profile in at least one spec reports
  violations.
* `2` — input error: missing `--contract`, malformed contract TOML,
  unresolvable target set, etc.

### Conditional-unaware MVP

The collector walks the spec AST and records every `BuildRequires:`
line, regardless of enclosing `%if` / `%ifarch`. This is intentional
for CI gating ("did anyone forget to declare a required build dep,
even behind a guard?") but it does mean a contract that asks for
`foo` on a RHEL profile will be satisfied by `BuildRequires: foo`
inside `%if 0%{?fedora}`. Branch-aware contract verification is a
follow-up; it will require deciding the semantics for "must `foo` be
required when `cond` is false?" which is policy-laden enough to
deserve its own design pass.

Rich/boolean deps in the source spec are flattened conservatively:
`And` / `Or` clauses register every named atom (so
`(gcc and make)` satisfies both `gcc` and `make`), while `If` /
`Unless` / `Not` arms are skipped — the conditional semantics there
are too policy-laden to interpret at lint time.

## Limitations of Phase 1

* No `matrix diff` / `impact` yet — those land in a future phase
  together with macro-usage span tracking.
* `matrix explain --line` does not distinguish individual branch
  bodies inside one `%if`/`%elif`/`%else` chain — all branches of an
  enclosing conditional are reported. For per-body activity the
  reader pairs the report with the directive lines in the spec.
* No parallel execution — profiles are analysed sequentially. Cost
  is linear in profile count; a profile set of 30 typically
  completes in a few seconds for one spec.
* Per-profile-override and target-wide defines share one
  `CliDefine` layer in the resolved profile, so `profile show` of
  a matrix-resolved profile can't yet distinguish the two sources
  from CLI `--define`.
