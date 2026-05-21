# CI integration

This page collects recipes for running `rpm-spec-tool` in CI. The
shape applies equally to GitHub Actions, GitLab CI, Jenkins, or
anything else that exposes a shell — the project's own CI uses GitHub
Actions and that's what the snippets target.

For the local pre-commit hook variant see
[workflow.md § Pre-commit (local)](workflow.md#pre-commit-local).

## Single-profile gate

The fastest path: one `check` call per spec, single target
distribution.

```yaml
# .github/workflows/spec-check.yml
name: spec-check
on: [push, pull_request]
jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Install rpm-spec-tool
        run: |
          curl -L https://github.com/johnlepikhin/rpm-spec-tool/releases/latest/download/rpm-spec-tool_X.Y.Z-1_amd64.deb \
            -o rpm-spec-tool.deb
          sudo apt-get install -y ./rpm-spec-tool.deb shellcheck
      - run: rpm-spec-tool check --profile rhel-9-x86_64 *.spec
```

Exit codes:

* `0` — clean.
* `1` — at least one deny-severity finding **or** `format --check`
  would rewrite the file.
* `2` — parse error, I/O, or CLI misuse.

`check` is `lint` + `format --check` rolled together — no `--fix`, no
mutation. It's the production gate.

## SARIF upload to GitHub Code Scanning

`lint --format sarif` produces a SARIF 2.1.0 document that the
`github/codeql-action/upload-sarif` action ingests directly:

```yaml
- name: Lint specs (SARIF)
  run: |
    rpm-spec-tool lint --format sarif --profile rhel-9-x86_64 *.spec \
      > spec.sarif
- name: Upload SARIF
  uses: github/codeql-action/upload-sarif@v3
  with:
    sarif_file: spec.sarif
```

After the first upload the findings show up under the repo's
**Security → Code scanning** tab, with their machine-applicable
suggestions attached.

For the multi-profile variant see [§ Matrix SARIF](#matrix-sarif).

## Promote warnings to errors

In strict CI you usually want any warning to fail:

```bash
rpm-spec-tool lint --deny warnings --profile rhel-9-x86_64 *.spec
```

`--deny warnings` (clippy convention) promotes every `warn` rule to
`deny`. Still pair-able with `--allow LINT` to silence specific
known-noisy rules.

## Matrix gating

When you ship to multiple distributions, declare a target set in
`rpmspec.toml` and replace `check` with `matrix check`:

```toml
[targets.release-2026]
profiles = [
  "rhel-8-x86_64",
  "rhel-9-x86_64",
  "altlinux-10-x86_64",
  "sles-15-x86_64",
]
```

```yaml
- name: Matrix lint
  run: |
    rpm-spec-tool matrix check --target-set release-2026 *.spec
```

Findings aggregate — the same warning across N profiles surfaces as
**one** record with an `affected:` list. See
[matrix.md](matrix.md#aggregation-contract) for the aggregation
contract, and [§ Matrix SARIF](#matrix-sarif) below for the SARIF
shape with per-profile properties.

## Baseline mode — gate on *new* findings only

Legacy specs often carry a large backlog of warnings. Baseline mode
records the current state and makes CI gate on *new* regressions:

```bash
# One-time, locally:
rpm-spec-tool matrix baseline create \
  --target-set release-2026 *.spec --out baseline.json

# Commit baseline.json. Then in CI:
rpm-spec-tool matrix check \
  --target-set release-2026 \
  --baseline baseline.json \
  --fail-on new *.spec
```

* `--fail-on all` (default) — any deny finding fails.
* `--fail-on new` — only findings whose `matrix_signature` is **not**
  in the baseline contribute to a non-zero exit. Requires
  `--baseline`; without it the command exits 2 so a misconfigured CI
  step can't silently degrade to "every finding is new".

Full lifecycle, schema, and stability caveats:
[matrix.md § Baseline mode](matrix.md#baseline-mode).

## Matrix SARIF

```bash
rpm-spec-tool matrix check --format sarif \
  --target-set release-2026 *.spec > spec.sarif
```

Every SARIF `Result` carries per-profile and matrix metadata under
`properties`:

```json
"properties": {
  "profile": "rhel-9-x86_64",
  "target_set": "release-2026",
  "matrix_signature": "...",
  "affected_profiles": ["rhel-8-x86_64", "rhel-9-x86_64"]
}
```

SARIF tolerates arbitrary properties, so consumers that ignore them
still read the result list correctly.

## Repo-aware lints in CI

If you point profiles at RPM repositories, the tool can verify every
`BuildRequires:` / `Requires:` against real metadata
(`matrix deps check`), simulate upgrades (`matrix upgrade-sim`), or
solve the full buildroot closure (`matrix buildroot solve`). The
network policy matters:

| Mode                            | Network | Behaviour on cache miss      | When |
| ------------------------------- | ------- | ---------------------------- | ---- |
| `Offline` (default)             | no      | RPM-REPO-* lints skip + INFO | local dev, default CI |
| `--cache-only`                  | no      | **hard error**               | strict CI ("the cache must be populated") |
| `--allow-fetch`                 | yes     | fetch                        | `repo sync` only       |

Typical CI shape:

```yaml
- name: Restore repo cache
  uses: actions/cache@v4
  with:
    path: ~/.cache/rpm-spec-tool/
    key:  rpmspec-cache-${{ hashFiles('rpmspec.toml') }}

- name: Warm cache (on miss)
  run: rpm-spec-tool repo sync --target-set release-2026 --allow-fetch

- name: Repo-aware deps check
  run: |
    rpm-spec-tool matrix deps check \
      --target-set release-2026 \
      --cache-only *.spec
```

The cache layout — `$XDG_CACHE_HOME/rpm-spec-tool/repos/<sha>/snapshots/<rev>/`
with atomic writes and conditional GETs — is documented in
[repos.md § On-disk cache layout](repos.md#on-disk-cache-layout).

For a corporate mirror with a CA missing from the system trust store,
prefer installing it via `update-ca-trust` / `update-ca-certificates`.
Avoid `--insecure-tls` in production CI — it silently accepts any
server identity, including MITM and DNS hijack.

## Lint catalogue sanity check

The project's own CI runs a `lints-doc-check` job that regenerates
`doc/lints-list.md` from the registry and fails on drift. The same
gate is useful in any repo that vendors rule customisations — wire it
in:

```yaml
- name: Lints doc check
  run: |
    cargo run --release --quiet -p rpm-spec-tool -- \
      lints --format markdown > /tmp/lints-list.md
    diff -u doc/lints-list.md /tmp/lints-list.md
```

The pre-commit hook in [`.githooks/`](../.githooks/) does the same
locally — enable it with `git config core.hooksPath .githooks`.

## Spec-aware PR review

`matrix impact` answers *"this commit touched `myproject.spec`, which
platforms moved and how?"*:

```yaml
- name: Spec impact
  if: github.event_name == 'pull_request'
  run: |
    rpm-spec-tool matrix impact \
      --target-set release-2026 \
      --from origin/${{ github.base_ref }} --to HEAD \
      $(git diff --name-only origin/${{ github.base_ref }} ... HEAD | grep '\.spec$')
```

`--to` defaults to the working tree (uncommitted edits), so the same
command works locally. Behaviour and edge cases (file added in PR,
spec outside any git repo) live in
[matrix.md § Impact mode](matrix.md#impact-mode).

## Distributing the binary in CI

The project ships `.tar.gz` (just the binary), `.deb`, and `.rpm`
artifacts for `x86_64` and `aarch64` on every tag. Pick the one that
matches your runner:

* **Ubuntu** runners — `.deb` via `apt install ./*.deb`.
* **AlmaLinux / Rocky / RHEL-derivative** runners — `.rpm` via
  `dnf install ./*.rpm`.
* **Container** images — extract the `.tar.gz` to `/usr/local/bin/`.

Every tag also publishes a `SHA256SUMS` file; verify it in your
install step if your CI policy demands it.

For Rust builds, `cargo install --git ... --tag vX.Y.Z` works on any
runner with a Rust toolchain — slower than the pre-built artifacts,
but immune to packaging quirks.

## Troubleshooting

* **`shellcheck` errors not appearing.** The binary is an optional
  dependency. Install via `apt install shellcheck` /
  `dnf install ShellCheck` — the package's `Recommends` won't pull it
  in on its own. Confirm with `rpm-spec-tool lints --severity warn |
  grep RPM200`.
* **`format --check` keeps failing in CI but passes locally.** Some
  editor is round-tripping the file. Pin `[format]` in
  `rpmspec.toml` and (optionally) bake `rpm-spec-tool format
  --in-place` into the pre-commit hook.
* **Matrix run says "target set unknown".** `rpmspec.toml` lives at
  a different level than CI expects — pass `--config PATH` explicitly,
  or run `rpm-spec-tool target list` in CI as a one-time debug step.
* **Baseline drift after a Rust toolchain upgrade.** `matrix_signature`
  uses the stdlib hasher, which is *stable within one binary release*
  but not contractually stable across `rustc` upgrades. Refresh the
  baseline once after a major Rust bump. See
  [matrix.md § Stability caveat](matrix.md#stability-caveat).
