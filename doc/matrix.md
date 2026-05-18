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

On Windows, the underlying `tempfile::NamedTempFile::persist` call uses
`MoveFile` *without* `MOVEFILE_REPLACE_EXISTING`, so persisting onto an
already-existing `PATH` fails. To refresh a baseline on Windows, delete
the destination first (`del baseline.json`) or write to a fresh path.
POSIX has no such restriction — successive runs overwrite cleanly.

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

Output (human, interactive TTY):

```text
Matrix coverage: target set `product-2026q2` (5 profiles)
  tags:  [ALWAYS] every profile activates  ·  [DEAD] no profile activates under any variant
         [CONDITIONAL: M=V] inactive under current build but reachable under declared [macros.M]
         [INDET] evaluator couldn't decide (see indeterminate-reason rollup below)
         (no tag) verdicts differ across profiles — see `under current build:` block

==> product.spec
    4 branches: 0 always · 0 conditional · 1 dead · 1 indeterminate · 2 mixed
    [config] undefined-macro        (1 branches)  undefined macro: suse_version

  line 9: %if 0%{?rhel}
    under current build:
      active:   rhel-8-x86_64, rhel-9-x86_64
      inactive: altlinux-10-x86_64, sles-15-x86_64

  line 13: %if 0 [DEAD]
  line 17: %ifarch e2k
    under current build:
      active:   altlinux-10-e2k
      inactive: altlinux-10-x86_64, rhel-8-x86_64, rhel-9-x86_64, sles-15-x86_64

```

The first line block — `tags: …` — is the one-shot legend printed
only when stdout is an interactive terminal. Pipes / redirects
suppress it so `grep`/`awk` consumers parse a stable line set.

A per-spec summary header reports every verdict class so operators
don't have to scan the body to know what's there. The
`[config] undefined-macro …` rollup line aggregates indeterminate
reasons across all branches; `[config]` reasons are operator-fixable
(declare `[macros.X]` or fix the profile), `[tool]` reasons are
evaluator limitations that require a code change.

Tags:

* `[DEAD]` — branch is inactive on every profile and no
  variant rescues it. The whole `%if…%endif` block is dead code.
* `[ALWAYS]` — branch is active on every profile. The condition
  has no effect; the body can be inlined.
* `[INDET]` — every profile produced an indeterminate verdict.
  The `indeterminate:` follow-up line names the reason and
  category (`[config]`/`[tool]`).
* `[CONDITIONAL: macro=value]` — reachable under at least one
  declared variant. See § "Macro variants" below.
* No tag — verdicts differ across profiles (some active, some
  inactive). The renderer expands the branch into an `under
  current build:` block listing the per-bucket profile sets.

Branch density: `[DEAD]`, `[ALWAYS]`, and `[CONDITIONAL]` that
covers all profiles render as a single line. `[INDET]` branches
render as two lines (header + reason). Verbose branches — mixed
verdicts and conditionals not fully covering the target set —
get an `under current build:` sub-block plus a trailing blank
line as a visual separator between adjacent verbose entries.

### Filter flags

* `--summary` — print the header + reason rollup only; skip the
  per-branch listing. The "is this spec healthy?" one-liner.
* `--only <dead|conditional|indeterminate|always|noisy>` — restrict
  the per-branch listing to one verdict class. `noisy` is the
  triage shorthand (everything except ALWAYS).

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

### Macro variants

Real-world specs select between *build editions* via macros the user
sets at build time (`-D edition ent`, `-D pgsql_major 15`). Coverage
without context can't distinguish:

* **Genuinely dead code** — a branch no plausible build configuration
  activates. Worth deleting.
* **Build-conditional code** — a branch inactive under the current
  build but reachable under another declared edition. Worth keeping.

Declaring the allowed value set of a macro in `.rpmspec.toml` lets
coverage tell those apart:

```toml
[macros.edition]
values = ["ent", "std", "1c"]
description = "Build edition selector"

[macros.pgsql_major]
values = ["13", "14", "15", "16", "17", "18"]
```

Each `[macros.<name>]` entry declares the value set for one macro.
The schema is intentionally identical-shape to `[profiles.<name>]`
and `[targets.<name>]` — same `name → table` model — so configs grow
uniformly.

When variants are declared, `matrix coverage` runs every branch that
is inactive on every profile through a *cartesian product* of the
declared variant values. A branch reachable on profile P under at
least one variant combination is tagged `[CONDITIONAL: macro=value]`
instead of `[DEAD]`. When the variant assignment covers every
profile in the target set, the tag itself carries the full
information; otherwise a `reachable when` line lists which profiles
the variant rescues.

```text
line 30: %if "%{edition}" == "1c" [CONDITIONAL: edition=1c]

line 491: %if 0%{?obsolete_distro_macro} [DEAD]
```

DEAD and ALWAYS branches render as a single line each — the tag
already conveys all four verdict buckets. CONDITIONAL and
mixed-verdict branches expand into the detailed
`active`/`inactive`/`reachable when`/`indeterminate` form only
when the tag alone doesn't tell the full story.

The first branch is build-conditional (some `-D edition` value
activates it). The second is genuinely dead — no declared variant
references `obsolete_distro_macro`, so the tool has no evidence it's
reachable.

#### Interaction with `-D`

* Without `-D` for a variant macro, coverage analyses every declared
  variant value of that macro.
* With `-D NAME VALUE` for a variant macro, the explicit value wins
  for the current build's verdict (`active_on` / `inactive_on`); the
  variant set still applies to the cartesian product for the
  reachability check.
* Values supplied via `-D` that are NOT in the declared variants
  produce a warning but are not rejected — operators always have the
  final word.

#### Cartesian-product cap

The cartesian product is bounded at 64 combinations per branch
(`MAX_VARIANT_COMBINATIONS`). Configurations exceeding the cap log a
`tracing::warn` and skip variant analysis for the affected branch
(keeping the pre-variant verdict). Operators who hit the cap can:

* Trim the variant value set for one macro.
* Split the analysis into separate target sets with narrower variant
  declarations.
* Treat the cap as a sign the spec deserves a refactor (genuinely
  needing 8⁴ combinations is rare).

#### JSON shape

Two additive fields land in each branch object:

```json
{
  "is_conditional": true,
  "conditional_on": ["rhel-9-x86_64"],
  "reachable_under": { "edition": ["1c"] }
}
```

`is_conditional` is `false` and the other two are omitted (via
`#[serde(skip_serializing_if)]`) when no variant analysis fired —
existing JSON consumers see no shape change unless they opt into the
new fields.

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

## Diff mode

`matrix diff` answers "what actually changes between profile A and
profile B?" by walking the spec branch-aware per profile and
comparing the resulting `BuildRequires` and `Requires` sets.

```bash
rpm-spec-tool matrix diff product.spec \
  --profiles rhel-9-x86_64,altlinux-10-x86_64
```

Exactly two distinct profiles. The diff is binary by design; for
N-profile classification use the future `matrix classes` command
(equivalence groups).

### Branch-aware semantics

Conditional resolution uses the *Skip* indeterminate policy:
deps inside an indeterminate branch (e.g. arithmetic the evaluator
can't model) appear in neither bucket. This is conservative —
under-reports rather than over-reports — and matches the PR-review
use case where a false "only on rhel-9" flag is more confusing
than a quietly-skipped uncertainty.

Rich/boolean dep flattening (`And` / `Or` / `With` conjunctive
forms; `Without` left-side; `If` / `Unless` skipped) uses the
shared `analyzer::dep_walk` walker — the same one
`matrix verify-contract` uses. Future consumers (artifact
comparator, etc.) plug into the same primitive so the rich-dep
policy stays in one place.

### Human output

```text
# Matrix diff: rhel-9-x86_64 vs altlinux-10-x86_64
## product.spec

  BuildRequires
    common (2): gcc, make
    only rhel-9-x86_64 (1): systemd-rpm-macros
    only altlinux-10-x86_64 (1): rpm-build-systemd

  Requires
    common (1): glibc
    only rhel-9-x86_64 (1): rhel-only-pkg
    only altlinux-10-x86_64 (0): (none)
```

Items are sorted alphabetically within each bucket. Count is shown
in parentheses for quick scanning.

### JSON output

```json
{
  "profile_a": "rhel-9-x86_64",
  "profile_b": "altlinux-10-x86_64",
  "path": "product.spec",
  "groups": [
    {
      "tag": "BuildRequires",
      "common": ["gcc", "make"],
      "only_a": ["systemd-rpm-macros"],
      "only_b": ["rpm-build-systemd"]
    },
    {
      "tag": "Requires",
      "common": ["glibc"],
      "only_a": ["rhel-only-pkg"],
      "only_b": []
    }
  ]
}
```

`tag` is the canonical Display form (`"BuildRequires"`,
`"Requires"`); buckets are sorted strings.

### Exit codes

* `0` — always (informational).
* `2` — usage error: not exactly two profiles, identical profiles,
  multiple input specs, etc.

### Limitations of Phase 1 diff

* Only `BuildRequires` and `Requires` are compared. `Provides` /
  `Conflicts` / `Obsoletes` are interesting too and additive to
  the JSON shape; deferred.
* Self-diff is rejected (`A,A`) — the resolver dedups by profile
  ID and the diff partition is empty anyway.
* Skip policy hides indeterminate branches. Use `matrix coverage`
  on the same target set to identify which branches were dropped.

## Impact mode

Where `matrix diff` compares two **profiles** at one revision,
`matrix impact` compares two **revisions** of one spec across every
profile in a target set. The PR-review use case: *"this commit
touches `foo.spec` — which platforms are materially affected and
which deps moved?"*.

```bash
rpm-spec-tool matrix impact \
  --target-set product-2026q2 \
  --from origin/main --to HEAD \
  product.spec
```

`--from` and `--to` accept anything `git show REV:path` accepts:
commit SHAs, branches, tags, `HEAD~3`. `--to` defaults to `HEAD` so
the common case "what's about to ship?" is a one-flag invocation.

### Mechanics

1. Resolve the spec's git repository root via
   `git -C <spec_dir> rev-parse --show-toplevel`.
2. Verify both `--from` and `--to` resolve to commits via
   `git rev-parse --verify REV^{commit}`. A typo'd SHA exits 2
   here rather than producing a misleading "deps added from scratch"
   diff later — git's error message is identical for "unknown rev"
   and "file missing at rev", so disambiguating up-front is the only
   safe option.
3. Fetch each side's spec body via `git show REV:relpath`.
4. Parse both and compute per-profile [`ProfileSignature`] dep sets
   with `IndeterminatePolicy::Skip` (same policy as `matrix diff`).
5. Set-diff per (profile, tag) pair into `added` / `removed` /
   `unchanged` buckets.

### File-missing semantics

If the spec doesn't exist at the `from` rev (a common PR-adds-new-spec
case), the CLI treats it as an empty document — every dep at `to`
surfaces as `added`. The file-missing detection covers all wordings
git has used: `"does not exist"`, `"exists on disk, but not in"`.

### Human output

```text
# Matrix impact: <from-sha> → <to-sha>, target set `product-2026q2` (3/5 profile(s) affected)
## product.spec

  rhel-9-x86_64: +1 -0
    BuildRequires
      added (1): cmake
  altlinux-10-x86_64: (no change)
  …
```

### JSON shape

```json
{
  "from": "<from-sha>",
  "to":   "<to-sha>",
  "target_set": "product-2026q2",
  "profiles": ["rhel-9-x86_64", "..."],
  "path": "product.spec",
  "affected_profile_count": 3,
  "per_profile": [
    {
      "profile_id": "rhel-9-x86_64",
      "tags": [
        {
          "tag_label": "BuildRequires",
          "changes": {
            "added": ["cmake"],
            "removed": [],
            "unchanged": ["gcc", "make"]
          }
        },
        { "tag_label": "Requires", "changes": { … } }
      ]
    }
  ]
}
```

### Exit codes

* `0` — always (informational; impact mode never gates CI directly —
  use `matrix check --fail-on new` for gating).
* `2` — usage error: stdin input, multiple spec paths, spec outside
  any git repo, unresolvable `--from`/`--to`, non-canonicalisable
  spec path.

### Limitations of Phase 1 impact

* Same tag set as `matrix diff`: only `BuildRequires` / `Requires`.
* Same skip-on-indeterminate policy. A branch that's
  indeterminate on one side but resolvable on the other still
  contributes — `matrix coverage` is the right tool to audit what
  was dropped on either side.
* Spec must be tracked in git. Reading two arbitrary on-disk files
  is not currently supported (deferred — the PR-review workflow
  always uses git).
* Reads stdout of `git show` — paths with control characters
  intermixed in the *file content* are fine (bytes pass through),
  but the dependency walker only surfaces UTF-8 stretches.

## Equivalence classes

`matrix classes` collapses a target set into distinct dependency
"flavours" — every profile that produces the same effective
`BuildRequires` + `Requires` after branch resolution lands in one
[`EquivalenceClass`]. The command surfaces a recommended minimal
representative build set (one profile per class), suitable for CI
gating systems that ask "do I really need to run 30 builds?".

```bash
rpm-spec-tool matrix classes product.spec \
  --target-set product-2026q2
```

In practice matrix sets collapse meaningfully — many arch×distro
tuples produce identical specs once `%if 0%{?rhel}` / `%bcond_with X`
etc. resolve. The actual collapse factor is spec-dependent: a
distro-portable spec might fold 10 profiles into 1 class, while a
heavily-gated one fragments to 1-class-per-profile. Running one
profile per class is sufficient for "does this spec compile?"
verification regardless of the magnitude.

### Scope of the signature

A profile's signature is the sorted union of:

* every `BuildRequires:` dep atom active on the profile, plus
* every `Requires:` dep atom active on the profile,

both walked via [`crate::dep_walk`] (same rich-dep policy as
`matrix diff` and `matrix verify-contract`) and gated by
[`IndeterminatePolicy::Skip`] (a branch the evaluator can't resolve
contributes nothing on either side — under uncertainty we err
toward fewer distinct classes, matching what an operator means by
"these look the same to me").

Files / Provides / Conflicts / Obsoletes are NOT in the signature —
they are post-build properties more naturally answered by an
artifact comparator (future phase). Including them here would
fragment classes on differences operators don't care about for the
"do I need to run this build?" question.

### Output

Human format ranks classes by descending member count and lists the
representative + every member + per-tag dep sets, then appends the
minimal representative build set:

```text
# Matrix classes: target set `product-2026q2` (10 profiles → 3 class(es))
## product.spec

## Class 1 (5 members, sig fbf33e85f365e561)
  representative: rhel-8-x86_64
  members:        rhel-8-aarch64, rhel-8-x86_64, rhel-9-aarch64, rhel-9-x86_64, redos-7-x86_64
  BuildRequires (4): gcc, make, rhel-only, systemd-rpm-macros
  Requires (1): glibc

## Class 2 (3 members, sig 077db16af651011b)
  representative: altlinux-10-aarch64
  members:        altlinux-10-aarch64, altlinux-10-e2k, altlinux-10-x86_64
  BuildRequires (3): gcc, make, rpm-build-systemd
  Requires (1): glibc

## Class 3 (2 members, sig 1c36da981eb40b91)
  representative: sles-15-aarch64
  members:        sles-15-aarch64, sles-15-x86_64
  BuildRequires (4): gcc, make, suse-only, systemd-rpm-macros
  Requires (1): glibc

## Minimal representative build set (3)
  rhel-8-x86_64
  altlinux-10-aarch64
  sles-15-aarch64
```

JSON output mirrors the structure plus envelope fields
(`target_set`, `profiles`, `path`, `class_count`, `classes`,
`representatives`).

### Stability caveat

The hex signature uses
[`std::collections::hash_map::DefaultHasher`] (currently SipHash 1-3
with a fixed seed) — same caveat as `MatrixSignature`. Stable within
one binary release, not contractually stable across stdlib upgrades.
CI baselines keyed on the hex string will need a refresh after a
major rustc bump.

**Class membership itself is hash-independent:** profiles are grouped
on the full structural `ProfileSignature` (sorted dep sets), not on
the hash. A hash collision can change the hex label assigned to a
class but cannot silently merge two distinct dep sets into one.

### Exit codes

* `0` — always (informational; never gates).
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

### Branch-aware verification

The verifier walks the spec **branch-aware**: a `BuildRequires:`
line inside `%if 0%{?fedora}` does NOT satisfy a contract on a RHEL
profile, because the evaluator marks that branch Inactive on RHEL
and the body is pruned during collection.

Each profile is checked under two projections of the spec:

* **required-side** uses the *Skip* indeterminate policy. A dep
  inside an indeterminate branch (e.g. `%if 0%{?rhel} >= 8` —
  arithmetic the evaluator can't model) does NOT count toward
  `must_have_buildrequires`. Conservative: prefer reporting
  "missing required" only when the dep is definitely missing.
* **forbidden-side** uses the *Include* indeterminate policy. A dep
  inside an indeterminate branch DOES count toward
  `must_not_have_buildrequires`. Conservative the other way:
  prefer flagging "forbidden present" if the dep might be there.

Net effect: the verifier never overlooks a forbidden dep that could
reach the build, and never spuriously fails a required dep that is
only declared inside an undecidable branch. Indeterminate branches
degrade gracefully toward "fail loud, not silent".

Rich/boolean deps in the source spec are flattened conservatively
via the shared `analyzer::dep_walk` walker: `And` / `Or` / `With`
conjunctive clauses register every named atom (so
`(gcc and make)` satisfies both `gcc` and `make`); `Without` keeps
the left side only; `If` / `Unless` arms are skipped — the
conditional semantics there are too policy-laden to interpret at
lint time. `matrix diff` uses the same walker so both commands
report the same atom set for the same source.

## Build conditions (`%bcond_with` / `%bcond_without`)

Modern RPM specs gate optional features via the `%bcond_*` family:

```rpm
%bcond_with bootstrap     # default OFF — enable with `rpmbuild --with bootstrap`
%bcond_without docs       # default ON  — disable with `rpmbuild --without docs`

%if %{with bootstrap}
BuildRequires: bootstrap-pkg
%endif
```

Real-world survey: in `systemd.spec` 27 of 45 conditionals use this
pattern. Before Phase 10 every `%{with X}` was treated as an
undefined macro `with` and the evaluator returned `Indeterminate`
for the entire chain. Phase 10 makes the analyzer bcond-aware:

* The collector walks every `BuildCondition` AST node and records
  the declared default (`%bcond_with` → off, `%bcond_without` → on).
* CLI flags `--with FEATURE` / `--without FEATURE` (mirroring
  `rpmbuild`) flip the declared defaults. Both are repeatable.
* The evaluator recognises `%{with NAME}` / `%{without NAME}`
  inside `%if` conditions and resolves them via the bcond map
  rather than the profile's macro registry.

### Scope

* Available on every matrix subcommand that walks conditionals:
  `matrix check`, `matrix coverage`, `matrix expand`, `matrix
  explain`, `matrix diff`, `matrix verify-contract`.
* Bcond declarations are spec-level, not profile-level. Per-profile
  bcond overrides (e.g. `[targets.X.with]` in `.rpmspec.toml`) are
  a future extension — additive on the on-disk schema.
* `%bcond NAME DEFAULT` (rpm ≥ 4.17.1, the 2-arg form):
  * Literal default `0` or `1` is honoured directly (`%bcond foo 1`
    behaves like `%bcond_without foo`; `%bcond foo 0` like
    `%bcond_with foo`).
  * Any non-literal default expression (`%bcond pcre2 %[expr]`,
    `%bcond foo %{?something}`, etc.) is reported as
    `Indeterminate` on every `%{with NAME}` use site — the evaluator
    surfaces `EvalError::Unsupported` with the actionable hint
    "pass `--with`/`--without` to force a state". CLI overrides
    collapse the indeterminate state to a concrete value.

### Example

```bash
# Default behaviour: bootstrap off, docs on.
rpm-spec-tool matrix coverage --profiles rhel-9-x86_64 product.spec

# Simulate "rpmbuild --with bootstrap --without docs".
rpm-spec-tool matrix coverage --profiles rhel-9-x86_64 \
  --with bootstrap --without docs product.spec
```

If both `--with FOO` and `--without FOO` are passed in the same
invocation, `--with` wins (matches what the analyzer's
`BcondMap::from_spec` enforces and what RPM does in practice when
parsing the CLI in declaration order).

A `--with FOO` for a bcond the spec doesn't declare is honoured
silently — operators sometimes ship a `--with` for a feature that
comes from an included `.spec` file or a corporate macro package.
The undeclared name is recorded in `BcondMap::unmatched_overrides()`
for downstream diagnostics; the CLI does not yet surface it as a
warning. Conflicting `--with FOO --without FOO` invocations DO
produce a stderr warning naming the conflicting bconds (the
resolver picks `--with`).

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
