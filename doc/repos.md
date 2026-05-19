# Repositories — guide

> Status: **M1 skeleton** (rpm-md fetch + on-disk cache + `repo sync`/`show`/`cache`). Repo-aware lints (`matrix deps check` etc.) land in M2; apt-rpm support, lockfile, and built-in defaults in M3+.

`rpm-spec-tool` can attach RPM repositories to each profile. With a configured repo set the tool stops being a pure-text linter and starts answering questions a release engineer asks before submitting a build:

- Will `BuildRequires:` resolve on this distribution?
- Will the new EVR actually be picked up as an upgrade against what's already in the repository?
- Does a file in `%files` already belong to another package?

This document covers what M1 ships: the configuration shape, the `repo` CLI subcommand, the on-disk cache layout, and the offline/online policy. Later milestones add lints, lockfile, apt-rpm support, and security scanning.

## Concepts

| Layer | What it is | Where it lives |
|-------|------------|----------------|
| **Profile** | The target distribution environment — identity, macros, rpmlib features, architecture, license whitelist. | `[profiles.<name>]` in `.rpmspec.toml` (plus built-ins). |
| **Repo set** | One or more RPM repositories attached to a profile. | `[profiles.<name>.repos.<id>]`. |
| **Buildroot** | Packages assumed installed in the chroot before `BuildRequires:` is processed. | `[profiles.<name>.buildroot]`. |
| **Snapshot** | One fetched copy of a repository's metadata, identified by the SHA-256 of `repodata/repomd.xml`. | `~/.cache/rpm-spec-tool/repos/<sha>/snapshots/<rev>/`. |
| **Repo universe** | The assembled, in-memory view of every package across the profile's repos, with Provides / Requires / Conflicts / Obsoletes / file-owner indexes. | Built on demand by `repo-resolver`. |

## Configuration

The TOML schema sits inside the existing `[profiles.<name>]` block. Field names follow `dnf`'s conventions so packagers recognise them.

```toml
[profile = "my-rhel-9"]                          # active profile

[profiles."my-rhel-9"]
extends = "rhel-9-x86_64"                        # built-in baseline

[profiles."my-rhel-9".repos.baseos]
baseurl  = "https://repo.example/rhel/9/BaseOS/$basearch/os/"
kind     = "rpm-md"                              # auto | rpm-md | apt-rpm
enabled  = true
priority = 10
role     = "base"                                # base | updates | product | internal | optional | source | debug
gpgcheck = true                                  # P0: warn-only; P1: enforce
gpgkey   = ["builtin:rhel-9-release"]

[profiles."my-rhel-9".repos.appstream]
baseurl = "https://repo.example/rhel/9/AppStream/$basearch/os/"
role    = "updates"

# Internal product repo: priority 5 so it wins ties against base/appstream
[profiles."my-rhel-9".repos.product]
baseurl  = "https://repo.example/product/rhel/9/$basearch/"
priority = 5
role     = "product"

[profiles."my-rhel-9".buildroot]
base_packages          = ["rpm-build", "redhat-rpm-config", "gcc", "make", "bash"]
implicit_buildrequires = []                      # shadow BRs, per-platform
```

### Layering

Repo entries layer through the same `extends → showrc → user override → CLI` chain as macros. Specifically:

- A repo with id `baseos` declared in a `[profiles.X]` that `extends` a built-in **replaces** the inherited entry wholesale (atomic — fields don't merge).
- Setting `enabled = false` on a repo that came from an inherited built-in **masks** it without forcing the user to also redefine its URL.
- The TOML key (`baseos`, `appstream`, …) must match `[a-z0-9_-]{1,64}` and is validated at config-load time so a typo fails fast rather than silently turning into an extra repo.

### URL placeholders

dnf-style placeholders are interpolated from the profile's identity and arch:

| Placeholder | Source | Example |
|-------------|--------|---------|
| `$basearch` | `Profile.arch.build_arch` | `x86_64`, `aarch64` |
| `$arch`     | alias for `$basearch` | — |
| `$releasever` | `Profile.identity.dist_tag` with `.el` / `.` stripped | `9`, `40` |
| `$infra`    | always `stock` in M1 | — |

Unknown placeholders are a hard error at fetch time, never a silent broken URL.

## CLI

### `repo sync`

The only command that touches the network. Always requires `--allow-fetch`.

```bash
# Sync the active profile's repos
rpm-spec-tool repo sync --allow-fetch

# Sync one specific profile
rpm-spec-tool repo sync --profile rhel-9-x86_64 --allow-fetch

# Sync every profile defined in .rpmspec.toml
rpm-spec-tool repo sync --all-profiles --allow-fetch

# Use an alternate cache directory (overrides $RPM_SPEC_TOOL_CACHE_DIR
# and the default $XDG_CACHE_HOME/rpm-spec-tool/)
rpm-spec-tool repo sync --cache-dir ./.rpmspec-cache --allow-fetch
```

Output lists each repo synced, the resolved revision, and where the snapshot was stored.

### `repo show`

Reads from the cache — never fetches. Default mode prints a summary; `--full`, `--package`, `--provides` zoom in.

```bash
# Summary: revision, fetched_at, package count, top-10 by installed size
rpm-spec-tool repo show --profile rhel-9-x86_64

# Information for one specific package across all enabled repos
rpm-spec-tool repo show --profile rhel-9-x86_64 --package cmake

# Filter packages whose Provides match a substring
rpm-spec-tool repo show --profile rhel-9-x86_64 --provides 'pkgconfig(libsystemd)'

# Full dump (warning: may print tens of thousands of lines)
rpm-spec-tool repo show --profile rhel-9-x86_64 --repo baseos --full
```

### `repo cache`

Inspect and manage the on-disk cache.

```bash
rpm-spec-tool repo cache ls           # list cached repos with snapshot counts
rpm-spec-tool repo cache gc           # keep 1 snapshot per repo (default)
rpm-spec-tool repo cache gc --keep 3  # keep last 3 snapshots
rpm-spec-tool repo cache prune        # wipe every cached repo
rpm-spec-tool repo cache prune --repo abc123def  # wipe one
```

## Offline by default

Every command other than `repo sync` runs in **offline** mode by default. That's a deliberate choice — lints in CI must never hit the network behind the user's back. The modes form a 3-state ladder:

| Mode | Network? | Cache miss behaviour | When to use |
|------|----------|----------------------|-------------|
| `Offline` (default) | no | RPM-REPO-* lints skip + one INFO line | local dev, default CI |
| `CacheOnly` (`--cache-only`) | no | hard error | CI invariant: "the cache must be populated" |
| `Online` (`--allow-fetch`) | yes | fetch | `repo sync`; explicit ad-hoc invocations |

Environment overrides: `RPM_SPEC_TOOL_OFFLINE=1`, `RPM_SPEC_TOOL_CACHE_ONLY=1`, `RPM_SPEC_TOOL_CACHE_DIR=/path`.

The HTTP client respects `HTTP_PROXY` / `HTTPS_PROXY` / `NO_PROXY` natively — no extra flag needed inside a corporate network.

## On-disk cache layout

```
$XDG_CACHE_HOME/rpm-spec-tool/
├── http/<sha256-url>/{body, meta.json}         # raw response bodies + ETag / Last-Modified sidecar
├── repos/<sha256-canonical-baseurl>/
│   ├── current -> snapshots/<rev>/             # symlink to the latest snapshot
│   ├── snapshots/<rev>/
│   │   ├── repomd.xml                          # raw, retained for re-hashing
│   │   ├── primary.xml / filelists.xml / updateinfo.xml
│   │   ├── index.bincode                       # parsed RepoIndex for fast reload
│   │   └── manifest.json                       # backend kind, fetched_at, bytes, sha
│   └── revisions.log
├── tmp/                                         # GC'd on startup
├── lockfiles.json                              # registry of known lockfile paths (for GC pin awareness)
└── version                                     # cache schema version
```

Key invariants:

- **Snapshot id = `sha256(repomd.xml)`** so two profiles configured against the same URL share one cache directory automatically (auto-dedup).
- **Atomic writes**: every snapshot is staged in `tmp/`, fsync'd, then renamed under `snapshots/<rev>/`.
- **Concurrent processes** serialise on a per-repo `fcntl` exclusive lock — two `repo sync` of the same URL will queue rather than race.
- **`bincode` reload** speeds up the second-and-later open of a cached snapshot by ~10×; on schema mismatch the cache transparently re-parses from the raw XML it retains.
- **Conditional GET**: every HTTP fetch sends `If-None-Match` / `If-Modified-Since` so repeated `repo sync` of an unchanged repo returns `304 Not Modified` and uses cached bytes.

## Troubleshooting

> **`error: repo sync needs network access — pass --allow-fetch to enable fetching`**
> By design — `repo sync` is the only path that may hit the network and it must be opted in explicitly.

> **`info: profile <name> has no repos configured`**
> The profile has no `[profiles.X.repos.*]` block. RPM-REPO-* lints will skip; the rest of the analyzer works unchanged.

> **`error: profile <name> has invalid repo id <id>: ...`**
> The TOML key under `[profiles.X.repos.<id>]` violated `[a-z0-9_-]{1,64}`. Lowercase the id and remove anything that isn't a-z / 0-9 / `-` / `_`.

> **`error: ... unknown URL placeholder $foo`**
> Only `$basearch`, `$arch`, `$releasever`, `$infra` are interpolated; anything else is treated as a typo.

> **`error: ... HTTP 404` on `repodata/repomd.xml`**
> The configured URL does not point at a rpm-md tree. Browse the URL in a HTTP client and confirm it ends in a directory that contains a `repodata/` subdirectory.

> **The cache feels too big**
> `repo cache gc` keeps 1 snapshot per repo by default. Override with `--keep N` if you need to keep history (e.g. for the upcoming `repo impact` command). `repo cache prune` wipes everything.

## What's not in M1

Tracked for later milestones:

- `matrix deps check`, `matrix buildroot solve`, `matrix upgrade-sim`, RPM-REPO-* lints — **M2**.
- `repo lock {create,update,verify,pin-closure,status}` lockfile workflow — **M2 PR5**.
- apt-rpm backend (ALT Linux) + ALT built-in defaults — **M3**.
- `repo health`, `repo impact`, `workspace build-order`, `repo security scan` — **M4 / M5**.
- LSP integration (hover "package X is in repo Y", inline unsat diagnostics) — **P3+**.

See [`/tmp/ideas.md`](file:///tmp/ideas.md) for the full architectural brief and the `/home/evgenii/.claude/plans/quirky-rolling-comet.md` plan file for the rollout schedule.
