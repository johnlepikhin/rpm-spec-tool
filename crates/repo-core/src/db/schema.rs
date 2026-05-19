//! Per-repo SQLite schema (v1).
//!
//! One database file per (repo, snapshot revision). The cache layout
//! pins each `repo.db` under `~/.cache/rpm-spec-tool/repos/<sha-baseurl>/
//! snapshots/<rev>/repo.db`, so cross-profile auto-dedup by baseurl works
//! at the filesystem level — no need to track repo ownership inside the
//! database itself.
//!
//! ## Design choices
//!
//! - **One `caps` table for every capability flavour** (provides,
//!   requires, conflicts, obsoletes, recommends, suggests, supplements,
//!   enhances). Discriminated by a `kind` column. Saves four tables and
//!   four parallel indexes; one composite `(name, kind)` index serves
//!   every lookup pattern the resolver issues.
//! - **No foreign-key cascade enforcement at the SQLite level** — the
//!   writer always rebuilds the database from scratch per snapshot, so
//!   delete cascades never fire in practice. `FOREIGN KEY` declarations
//!   are present for documentation but `PRAGMA foreign_keys` stays off
//!   to keep batch inserts fast.
//! - **WAL + 64 MiB page cache**: hot lookups (name → providers) hit
//!   the cache; cold queries pull pages from disk. Matches the
//!   "RAM stays bounded, hot data still fast" trade-off the SQLite
//!   refactor was undertaken for.
//! - **`meta` table** carries everything that was in `manifest.json`
//!   (schema version, revision, fetched_at, backend kind, baseurl
//!   sha) so a `repo.db` is self-describing without an external sidecar.

/// Current schema version. Bumped when the DDL below changes in any
/// non-additive way; older databases are evicted by the cache layer
/// and re-built from XML.
pub const SCHEMA_VERSION: u32 = 2;

/// Full DDL applied to a freshly created database.
pub const CREATE_SQL: &str = r"
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;

CREATE TABLE packages (
    pkg_id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name            TEXT    NOT NULL,
    epoch           INTEGER NOT NULL DEFAULT 0,
    version         TEXT    NOT NULL,
    release         TEXT    NOT NULL,
    arch            TEXT    NOT NULL,
    source_rpm      TEXT,
    summary         TEXT    NOT NULL DEFAULT '',
    size_installed  INTEGER NOT NULL DEFAULT 0,
    checksum_alg    TEXT    NOT NULL,
    checksum_hex    TEXT    NOT NULL,
    location        TEXT    NOT NULL DEFAULT ''
);
CREATE INDEX idx_packages_name ON packages(name);

CREATE TABLE caps (
    pkg_id  INTEGER NOT NULL REFERENCES packages(pkg_id),
    kind    TEXT    NOT NULL,
    name    TEXT    NOT NULL,
    flags   TEXT    NOT NULL,
    epoch   INTEGER,
    version TEXT,
    release TEXT
);
CREATE INDEX idx_caps_kind_name ON caps(kind, name);
-- (pkg_id, kind) composite supports `load_caps_by_kind(pkg_id, 'provides')`
-- in the resolver hot path; the older `(pkg_id)`-only index is kept
-- because `load_package` still scans by pkg_id across all kinds.
CREATE INDEX idx_caps_pkg       ON caps(pkg_id);
CREATE INDEX idx_caps_pkg_kind  ON caps(pkg_id, kind);

CREATE TABLE files (
    pkg_id INTEGER NOT NULL REFERENCES packages(pkg_id),
    path   TEXT    NOT NULL
);
CREATE INDEX idx_files_path ON files(path);
CREATE INDEX idx_files_pkg  ON files(pkg_id);

CREATE TABLE advisories (
    advisory_id TEXT PRIMARY KEY,
    severity    TEXT NOT NULL,
    payload     TEXT NOT NULL
) WITHOUT ROWID;
";

/// Reserved keys for the `meta` table.
pub mod meta_keys {
    pub const SCHEMA_VERSION: &str = "schema_version";
    pub const REPO_ID: &str = "repo_id";
    pub const REVISION: &str = "revision";
    pub const FETCHED_AT: &str = "fetched_at";
    pub const BACKEND_KIND: &str = "backend_kind";
    pub const BASEURL_SHA256: &str = "baseurl_sha256";
}

/// Capability kind discriminator stored in `caps.kind`. The set is
/// closed; new kinds require a schema bump.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CapKind {
    Provides,
    Requires,
    Conflicts,
    Obsoletes,
    Recommends,
    Suggests,
    Supplements,
    Enhances,
}

impl CapKind {
    /// Stable on-disk discriminator. NEVER change these strings —
    /// they are part of the schema contract.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Provides => "provides",
            Self::Requires => "requires",
            Self::Conflicts => "conflicts",
            Self::Obsoletes => "obsoletes",
            Self::Recommends => "recommends",
            Self::Suggests => "suggests",
            Self::Supplements => "supplements",
            Self::Enhances => "enhances",
        }
    }
}
