//! SQLite-backed per-repo metadata store.
//!
//! Replaces the bincode `index.bincode` snapshot for two reasons:
//!
//! 1. Loading the bincode form materialises the entire `RepoIndex`
//!    (every package, every capability, every file path) into RAM —
//!    around 5 GB peak for a profile spanning BaseOS / AppStream /
//!    Updates / a vendor PostgreSQL repo. SQLite with a bounded page
//!    cache stays under ~150 MB resident.
//! 2. Lookup queries (`name → providers`, `file → owner`) are O(1)
//!    on disk via the per-column indexes; no HashMap rebuild on every
//!    `matrix deps check` invocation.
//!
//! ## Lifecycle
//!
//! - [`RepoDb::create`] wipes any existing file at the path and lays
//!   down the schema. Called by `repo sync` after parsing XML.
//! - [`RepoDb::open`] opens an existing database read-only-ish (we
//!   accept the default rusqlite open mode; the schema is never
//!   mutated by readers). Called by every lookup-side consumer.
//! - [`RepoDb::ingest_packages`] streams a parsed [`crate::Package`]
//!   batch into the database inside a single transaction. Designed
//!   for one bulk write per sync — incremental updates are not
//!   supported (whole-repo re-sync replaces the file).
//!
//! ## Concurrency
//!
//! Single writer at a time (the existing `fcntl` snapshot lock in
//! `repo-metadata::locks` already serialises). Multiple readers are
//! fine because SQLite's WAL mode tolerates concurrent reads.
//!
//! ## Adding a column / schema change procedure
//!
//! There is intentionally **no in-place migration story**. A repo
//! cache is a *snapshot* — if the schema changes, the right answer is
//! to re-fetch the upstream metadata into a fresh database. Cache
//! files written under an older schema are detected at open time
//! (see [`schema::SCHEMA_VERSION`]) and the user is told to re-sync.
//!
//! The runbook for adding (or removing, or renaming) a column:
//!
//! 1. **Bump [`schema::SCHEMA_VERSION`]** by one. This is the *only*
//!    knob — readers compare strictly against this constant and reject
//!    older or newer cache files via [`RepoError::Database`]. There
//!    are no compatibility shims and we don't want any.
//! 2. **Edit `schema::CREATE_SQL`** to include the new column (or
//!    drop / rename the old one). The full DDL lives in one string so
//!    a code review can see the whole schema in one diff.
//! 3. **Update the writer half** in [`RepoDb::ingest_packages`] (and
//!    its `convert::*` helpers) to populate the new column from a
//!    field on [`Package`]. If the column maps to a struct field that
//!    didn't exist before, extend [`Package`] / [`NEVRA`] first and
//!    propagate.
//! 4. **Update every reader half** — and there are two parallel
//!    families that are easy to confuse:
//!    - `load_package`'s inline row closure, `load_package_with_files`,
//!      `load_nevra`, plus the `pkg_briefs_*` helpers
//!      (`pkg_briefs_by_name`, `pkg_briefs_by_source_name`,
//!      `pkg_briefs_providing`) which share the free function
//!      `read_pkg_nevra_row`.
//!    - The brief-style readers (`top_n_by_size`, `all_packages_brief`,
//!      `package_brief`, `packages_by_name_brief`, `file_owners`,
//!      `files_for_pkg`) which use `PackageBrief::from_row`.
//!
//!    **This is the most common bug class:** the writer fills the
//!    column but one reader still reconstructs with the default,
//!    producing silent data loss until a user notices the field is
//!    empty in a diagnostic. Grep for every `row.get` in this file
//!    after a column change to confirm coverage.
//! 5. **Add a round-trip test** in the `tests` mod at the bottom of
//!    this file. The pattern is `ingest_packages([sample_pkg(…)])` →
//!    `load_package(id)` → assert every new field matches. The
//!    existing `create_open_roundtrip` test is the model.
//! 6. **Schema bumps are user-visible breakage** — every user must
//!    re-run `rpm-spec-tool repo sync` after upgrading past the
//!    bump. Note it in the CHANGELOG and verify the open-error
//!    message stays actionable.

mod convert;
mod schema;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension, params};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::RepoError;
use crate::index::RepoId;
use crate::package::{Capability, NEVRA, Package, PkgChecksum};

pub use schema::{CapKind, SCHEMA_VERSION};

const DEFAULT_FILE_NAME: &str = "repo.db";

/// One per-repo metadata database.
///
/// Wraps `rusqlite::Connection` in a `Mutex` so the type is `Sync`.
/// The analyzer wraps `RepoUniverse` (which carries a `Vec<RepoDb>`)
/// in `Arc<>` and the `Lint` trait requires `Send` on rule structs —
/// `Arc<T>: Send` demands `T: Send + Sync`, hence the mutex. In
/// single-threaded use (the production case) the lock is always
/// uncontended; the `Mutex` overhead is one atomic CAS per query.
#[derive(Debug)]
pub struct RepoDb {
    conn: Mutex<Connection>,
    path: PathBuf,
    /// Cached at open/create time so `load_package` can stamp it
    /// onto the result without acquiring a second lock (the
    /// `meta` lookup path would otherwise deadlock the existing
    /// guard).
    cached_repo_id: RepoId,
}

impl RepoDb {
    /// Canonical filename for the database inside a snapshot dir.
    #[must_use]
    pub fn file_name() -> &'static str {
        DEFAULT_FILE_NAME
    }

    /// Create a fresh database at `path`, wiping anything already
    /// there. Applies the v1 schema and seeds the `meta` table with
    /// the supplied identity fields.
    pub fn create(
        path: impl Into<PathBuf>,
        repo_id: &RepoId,
        revision: &str,
        fetched_at: OffsetDateTime,
        backend_kind: &str,
        baseurl_sha256: &str,
    ) -> Result<Self, RepoError> {
        let path = path.into();
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        let conn = Connection::open(&path)?;
        // Ingest-mode pragmas: the fresh DB is invisible to readers
        // (we write to a `.tmp` path and atomic-rename at the end).
        // Crash-safety is therefore "either we rename or we don't" —
        // SQLite's own journal + WAL machinery is pure overhead
        // here, and `synchronous = OFF` plus `journal_mode = OFF`
        // shave ~80 % off `repo sync` wall time on a 60k-package
        // mirror. `page_size = 16384` (4× the default) lets each
        // 4 KiB pwrite-friendly cache spill carry 4× more package
        // rows before flushing to disk. Live readers re-open via
        // [`Self::open`] which restores WAL + NORMAL via
        // [`apply_read_pragmas`].
        apply_ingest_pragmas(&conn)?;
        conn.execute_batch(schema::CREATE_SQL)?;

        let fetched_at_str = fetched_at
            .format(&Rfc3339)
            .map_err(|e| RepoError::Database(format!("rfc3339 format: {e}")))?;
        let mut stmt =
            conn.prepare("INSERT INTO meta (key, value) VALUES (?1, ?2)")?;
        stmt.execute(params![
            schema::meta_keys::SCHEMA_VERSION,
            schema::SCHEMA_VERSION.to_string()
        ])?;
        stmt.execute(params![schema::meta_keys::REPO_ID, repo_id.as_str()])?;
        stmt.execute(params![schema::meta_keys::REVISION, revision])?;
        stmt.execute(params![schema::meta_keys::FETCHED_AT, fetched_at_str])?;
        stmt.execute(params![schema::meta_keys::BACKEND_KIND, backend_kind])?;
        stmt.execute(params![schema::meta_keys::BASEURL_SHA256, baseurl_sha256])?;
        drop(stmt);

        Ok(Self {
            conn: Mutex::new(conn),
            path,
            cached_repo_id: repo_id.clone(),
        })
    }

    /// Create an in-memory database. Same schema, but tied to one
    /// connection (you cannot re-open it). Useful for tests and tiny
    /// fixture universes — production always uses [`Self::create`]
    /// with a file path.
    pub fn create_in_memory(
        repo_id: &RepoId,
        revision: &str,
        fetched_at: OffsetDateTime,
        backend_kind: &str,
        baseurl_sha256: &str,
    ) -> Result<Self, RepoError> {
        let conn = Connection::open_in_memory()?;
        apply_read_pragmas(&conn)?;
        conn.execute_batch(schema::CREATE_SQL)?;
        let fetched_at_str = fetched_at
            .format(&Rfc3339)
            .map_err(|e| RepoError::Database(format!("rfc3339 format: {e}")))?;
        let mut stmt = conn.prepare("INSERT INTO meta (key, value) VALUES (?1, ?2)")?;
        stmt.execute(params![
            schema::meta_keys::SCHEMA_VERSION,
            schema::SCHEMA_VERSION.to_string()
        ])?;
        stmt.execute(params![schema::meta_keys::REPO_ID, repo_id.as_str()])?;
        stmt.execute(params![schema::meta_keys::REVISION, revision])?;
        stmt.execute(params![schema::meta_keys::FETCHED_AT, fetched_at_str])?;
        stmt.execute(params![schema::meta_keys::BACKEND_KIND, backend_kind])?;
        stmt.execute(params![schema::meta_keys::BASEURL_SHA256, baseurl_sha256])?;
        drop(stmt);
        Ok(Self {
            conn: Mutex::new(conn),
            path: PathBuf::from(":memory:"),
            cached_repo_id: repo_id.clone(),
        })
    }

    /// Open an existing database for reading + (cheap) writes. Verifies
    /// the schema version matches; mismatched versions surface as
    /// [`RepoError::Database`] so the caller can evict + re-sync.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, RepoError> {
        let path = path.into();
        let conn = Connection::open(&path)?;
        apply_read_pragmas(&conn)?;

        let version: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![schema::meta_keys::SCHEMA_VERSION],
                |row| row.get(0),
            )
            .optional()?;

        let cached_repo_id = match conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![schema::meta_keys::REPO_ID],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            Some(s) => {
                // Cross-trust boundary: anything in `meta.repo_id` on
                // disk could have been tampered with. Re-validate
                // against the same `[a-z0-9_-]{1,64}` rule the
                // profile loader enforces on TOML keys, so a hostile
                // cache file can't smuggle ANSI escapes, newlines,
                // NUL bytes, or oversized strings into the
                // `tracing` / `eprintln!` lines that interpolate
                // `repo_id` (see `cli::commands::repo::sync`).
                if let Err(reason) = rpm_spec_profile::repos::validate_repo_id(&s) {
                    return Err(RepoError::Database(format!(
                        "repo db at {} has malformed meta.repo_id: {reason} — re-run `repo sync` to rebuild the cache",
                        path.display()
                    )));
                }
                RepoId::from(s)
            }
            None => {
                return Err(RepoError::Database(format!(
                    "repo db at {} is missing meta.repo_id",
                    path.display()
                )));
            }
        };

        match version.as_deref() {
            Some(v) if v == SCHEMA_VERSION.to_string() => Ok(Self {
                conn: Mutex::new(conn),
                path,
                cached_repo_id,
            }),
            Some(other) => Err(RepoError::Database(format!(
                "repo db at {} has schema {other}, expected {SCHEMA_VERSION}",
                path.display()
            ))),
            None => Err(RepoError::Database(format!(
                "repo db at {} is missing meta.schema_version",
                path.display()
            ))),
        }
    }

    /// Filesystem path the database lives at. Useful for diagnostics
    /// and cache GC.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read one meta key. Returns `None` if the key isn't set.
    pub fn meta(&self, key: &str) -> Result<Option<String>, RepoError> {
        let guard = self.lock();
        Ok(guard
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Convenience: read [`schema::meta_keys::REPO_ID`]. O(1) — the
    /// value is cached at open/create time.
    pub fn repo_id(&self) -> Result<RepoId, RepoError> {
        Ok(self.cached_repo_id.clone())
    }

    /// Convenience: read [`schema::meta_keys::REVISION`].
    pub fn revision(&self) -> Result<String, RepoError> {
        self.meta(schema::meta_keys::REVISION)?
            .ok_or_else(|| RepoError::Database("meta.revision missing".into()))
    }

    /// Convenience: read [`schema::meta_keys::FETCHED_AT`] as a parsed
    /// [`OffsetDateTime`].
    pub fn fetched_at(&self) -> Result<OffsetDateTime, RepoError> {
        let raw = self
            .meta(schema::meta_keys::FETCHED_AT)?
            .ok_or_else(|| RepoError::Database("meta.fetched_at missing".into()))?;
        OffsetDateTime::parse(&raw, &Rfc3339)
            .map_err(|e| RepoError::Database(format!("meta.fetched_at parse: {e}")))
    }

    /// Acquire the inner connection. Recovers from a poisoned mutex
    /// via `into_inner()` — Connection queries don't mutate
    /// user-visible state, so reading from a poisoned lock is safe
    /// and avoids panic-cascading through the lint session.
    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Bulk-insert packages in a single transaction. Returns the
    /// number of packages written. Designed for the one-shot `repo
    /// sync` writer; do not call repeatedly on the same database.
    pub fn ingest_packages(&mut self, packages: &[Package]) -> Result<usize, RepoError> {
        let mut guard = self.lock();
        let tx = guard.transaction()?;
        {
            let mut insert_pkg = tx.prepare(
                "INSERT INTO packages \
                 (name, epoch, version, release, arch, source_rpm, source_name, summary, \
                  size_installed, checksum_alg, checksum_hex, location) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            )?;
            let mut insert_cap = tx.prepare(
                "INSERT INTO caps \
                 (pkg_id, kind, name, flags, epoch, version, release) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            let mut insert_file = tx.prepare("INSERT INTO files (pkg_id, path) VALUES (?1, ?2)")?;

            for pkg in packages {
                let (alg, hex) = convert::checksum_columns(&pkg.checksum);
                let source_name =
                    pkg.source_rpm.as_deref().and_then(convert::source_rpm_name);
                insert_pkg.execute(params![
                    pkg.nevra.name.as_ref(),
                    pkg.nevra.epoch,
                    pkg.nevra.version.as_ref(),
                    pkg.nevra.release.as_ref(),
                    pkg.nevra.arch.as_ref(),
                    pkg.source_rpm.as_deref(),
                    source_name,
                    pkg.summary.as_ref(),
                    pkg.size_installed as i64,
                    alg,
                    hex,
                    pkg.location.as_ref(),
                ])?;
                let pkg_id = tx.last_insert_rowid();

                insert_cap_batch(&mut insert_cap, pkg_id, CapKind::Provides, &pkg.provides)?;
                insert_cap_batch(&mut insert_cap, pkg_id, CapKind::Requires, &pkg.requires)?;
                insert_cap_batch(&mut insert_cap, pkg_id, CapKind::Conflicts, &pkg.conflicts)?;
                insert_cap_batch(&mut insert_cap, pkg_id, CapKind::Obsoletes, &pkg.obsoletes)?;
                insert_cap_batch(&mut insert_cap, pkg_id, CapKind::Recommends, &pkg.recommends)?;
                insert_cap_batch(&mut insert_cap, pkg_id, CapKind::Suggests, &pkg.suggests)?;
                insert_cap_batch(&mut insert_cap, pkg_id, CapKind::Supplements, &pkg.supplements)?;
                insert_cap_batch(&mut insert_cap, pkg_id, CapKind::Enhances, &pkg.enhances)?;

                for path in &pkg.files {
                    insert_file.execute(params![pkg_id, path.as_ref()])?;
                }
            }
        }
        tx.commit()?;
        Ok(packages.len())
    }

    /// Number of packages indexed. Cheap (single COUNT).
    pub fn package_count(&self) -> Result<u64, RepoError> {
        let guard = self.lock();
        Ok(guard.query_row("SELECT COUNT(*) FROM packages", [], |row| row.get::<_, i64>(0))? as u64)
    }

    /// Look up packages by exact name. Returns pkg_id values; resolve
    /// to full [`Package`] via [`Self::load_package`].
    pub fn pkg_ids_by_name(&self, name: &str) -> Result<Vec<i64>, RepoError> {
        let guard = self.lock();
        let mut stmt = guard.prepare_cached("SELECT pkg_id FROM packages WHERE name = ?1")?;
        let rows = stmt.query_map(params![name], |row| row.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Combined `(pkg_id, NEVRA)` lookup for every package whose
    /// own name equals `name`. One round-trip; resolver uses this
    /// to avoid issuing a separate `load_nevra` per candidate in
    /// its tie-break ordering.
    pub fn pkg_briefs_by_name(&self, name: &str) -> Result<Vec<(i64, NEVRA)>, RepoError> {
        let guard = self.lock();
        let mut stmt = guard.prepare_cached(
            "SELECT pkg_id, name, epoch, version, release, arch \
             FROM packages WHERE name = ?1",
        )?;
        let rows = stmt.query_map(params![name], read_pkg_nevra_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Every binary package built from the source RPM with this
    /// `source_name` — i.e. every subpackage a given spec produced.
    ///
    /// `source_name` is the pre-parsed source-package identity stored
    /// in `packages.source_name` (`foo` from `foo-1.2-3.src.rpm`).
    /// `matrix upgrade-sim` and the RPM-REPO-030/031 lints use this
    /// to find the current published NEVRA for a spec's `Name:`.
    pub fn pkg_briefs_by_source_name(
        &self,
        source_name: &str,
    ) -> Result<Vec<(i64, NEVRA)>, RepoError> {
        let guard = self.lock();
        let mut stmt = guard.prepare_cached(
            "SELECT pkg_id, name, epoch, version, release, arch \
             FROM packages WHERE source_name = ?1",
        )?;
        let rows = stmt.query_map(params![source_name], read_pkg_nevra_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Combined `(pkg_id, NEVRA)` lookup for every package whose
    /// `Provides:` declares the capability `name`. Companion to
    /// [`Self::pkg_briefs_by_name`].
    pub fn pkg_briefs_providing(&self, name: &str) -> Result<Vec<(i64, NEVRA)>, RepoError> {
        let guard = self.lock();
        let mut stmt = guard.prepare_cached(
            "SELECT DISTINCT p.pkg_id, p.name, p.epoch, p.version, p.release, p.arch \
             FROM packages p JOIN caps c ON c.pkg_id = p.pkg_id \
             WHERE c.kind = 'provides' AND c.name = ?1",
        )?;
        let rows = stmt.query_map(params![name], read_pkg_nevra_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Cheap "does this package satisfy this requirement?" check
    /// for the resolver's hot path. Equivalent to
    /// `provides_satisfies(&load_package(pkg_id)?, req)` but
    /// without materialising the full caps + files lists. Returns
    /// `true` iff:
    ///
    /// * the package's own NAME equals `req.name` AND the NEVRA
    ///   satisfies the requirement's flag/EVR (if any), OR
    /// * the package declares a `Provides:` row whose NAME equals
    ///   `req.name` AND (for versioned requirements) whose EVR
    ///   satisfies the flag.
    ///
    /// File-path requirements (`req.name` starts with `/`) MUST
    /// NOT go through this — the resolver short-circuits those via
    /// `file_owner` upstream.
    pub fn package_satisfies(
        &self,
        pkg_id: i64,
        req: &crate::package::Capability,
    ) -> Result<bool, RepoError> {
        let guard = self.lock();
        let nevra = guard
            .prepare_cached(
                "SELECT name, epoch, version, release, arch FROM packages \
                 WHERE pkg_id = ?1",
            )?
            .query_row(params![pkg_id], |row| {
                let name: String = row.get(0)?;
                let epoch: u32 = row.get(1)?;
                let version: String = row.get(2)?;
                let release: String = row.get(3)?;
                let arch: String = row.get(4)?;
                Ok(NEVRA {
                    name: Arc::from(name),
                    epoch,
                    version: Arc::from(version),
                    release: Arc::from(release),
                    arch: Arc::from(arch),
                })
            })
            .optional()?;
        let Some(nevra) = nevra else {
            return Ok(false);
        };
        // Name match against the package's primary NEVRA — Unversioned
        // requires accept any provider; versioned ones check via
        // `CapVersion::matches(compare_rpm)`. The new enum collapses
        // the old `flags == None` / `evr.is_some()` two-step into one.
        if nevra.name.as_ref() == req.name.as_ref() {
            if req.version.is_unversioned() {
                return Ok(true);
            }
            if let Some(req_evr) = req.version.evr() {
                let prov_evr = nevra.evr();
                if req.version.matches(prov_evr.compare_rpm(req_evr)) {
                    return Ok(true);
                }
            }
        }
        let mut stmt = guard.prepare_cached(
            "SELECT epoch, version, release FROM caps \
             WHERE pkg_id = ?1 AND kind = 'provides' AND name = ?2",
        )?;
        let rows = stmt.query_map(params![pkg_id, req.name.as_ref()], |row| {
            let epoch: Option<u32> = row.get(0)?;
            let version: Option<String> = row.get(1)?;
            let release: Option<String> = row.get(2)?;
            Ok((epoch, version, release))
        })?;
        for r in rows {
            let (epoch, version, release) = r?;
            if req.version.is_unversioned() {
                return Ok(true);
            }
            let Some(req_evr) = req.version.evr() else {
                continue;
            };
            let (Some(v), Some(r)) = (version.as_deref(), release.as_deref()) else {
                continue;
            };
            let prov_evr = crate::evr::EVR::new(epoch, v, r);
            if req.version.matches(prov_evr.compare_rpm(req_evr)) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Top-N packages by installed size (largest first). Used by the
    /// default `repo show` view.
    pub fn top_n_by_size(&self, limit: u32) -> Result<Vec<PackageBrief>, RepoError> {
        let guard = self.lock();
        let mut stmt = guard.prepare(
            "SELECT name, epoch, version, release, arch, size_installed, location \
             FROM packages ORDER BY size_installed DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], PackageBrief::from_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Every package as a brief, ordered by name then EVR (deterministic
    /// for the `--full` dump).
    pub fn all_packages_brief(&self) -> Result<Vec<PackageBrief>, RepoError> {
        let guard = self.lock();
        let mut stmt = guard.prepare(
            "SELECT name, epoch, version, release, arch, size_installed, location \
             FROM packages ORDER BY name, epoch, version, release",
        )?;
        let rows = stmt.query_map([], PackageBrief::from_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Slim package summary for one specific `pkg_id`. Pairs with
    /// [`Self::file_owner`] in the file-path lookup flow:
    /// `file_owner` returns the owner's pkg_id; this method
    /// materialises the brief without forcing the caller to thread
    /// a name lookup or `load_package` JOIN.
    pub fn package_brief(&self, pkg_id: i64) -> Result<Option<PackageBrief>, RepoError> {
        let guard = self.lock();
        Ok(guard
            .query_row(
                "SELECT name, epoch, version, release, arch, size_installed, location \
                 FROM packages WHERE pkg_id = ?1",
                params![pkg_id],
                PackageBrief::from_row,
            )
            .optional()?)
    }

    /// Packages whose own name matches `name` exactly. Brief form,
    /// suitable for `repo show --package NAME` output.
    pub fn packages_by_name_brief(&self, name: &str) -> Result<Vec<PackageBrief>, RepoError> {
        let guard = self.lock();
        let mut stmt = guard.prepare(
            "SELECT name, epoch, version, release, arch, size_installed, location \
             FROM packages WHERE name = ?1 ORDER BY epoch DESC, version DESC, release DESC",
        )?;
        let rows = stmt.query_map(params![name], PackageBrief::from_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Packages whose `Provides:` list carries a capability whose
    /// name equals `name` exactly. Returned paired with the
    /// matching capability (always == `name` here, but kept for
    /// API symmetry with [`Self::packages_providing_like`]).
    pub fn packages_providing_exact(
        &self,
        name: &str,
        limit: u32,
    ) -> Result<Vec<(PackageBrief, String)>, RepoError> {
        let guard = self.lock();
        let mut stmt = guard.prepare(
            "SELECT p.name, p.epoch, p.version, p.release, p.arch, \
                    p.size_installed, p.location, c.name \
             FROM packages p JOIN caps c ON c.pkg_id = p.pkg_id \
             WHERE c.kind = 'provides' AND c.name = ?1 \
             ORDER BY p.name LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![name, limit], |row| {
            let brief = PackageBrief::from_row(row)?;
            let cap_name: String = row.get(7)?;
            Ok((brief, cap_name))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Packages whose `Provides:` list contains a capability whose
    /// name matches the SQL LIKE pattern, paired with the matching
    /// capability name. `repo show --provides-like PAT` uses this
    /// with `%PAT%` to perform a substring match.
    ///
    /// Ordering puts exact matches (`c.name = pattern_without_wildcards`)
    /// first so the canonical provider lands at the top of the
    /// rendered list — otherwise an alphabetic sort would push
    /// `cmake` itself below uppercase-named `Box2D-devel`,
    /// `CGAL-devel`, etc. that merely expose virtual capabilities
    /// like `cmake(Box2D)`.
    ///
    /// `exact_match` is used only for ordering; pass the
    /// substring-free form (typically the operator's input verbatim).
    ///
    /// # Caller escaping contract
    ///
    /// `pattern` is bound straight into a `LIKE ?1 ESCAPE '\'`
    /// expression, so the **caller** must escape every literal `%`,
    /// `_`, and `\` in user input by prefixing each with a backslash
    /// **before** wrapping it in `%…%`. Failing to escape lets a user
    /// search for `lib_foo` and accidentally match `libXfoo` /
    /// `lib1foo` (the `_` wildcard), or worse, scan the entire
    /// `caps` table for `%`. `repo show --provides-like` performs
    /// this escaping with a `replace('\\') → replace('%') →
    /// replace('_')` chain — the `\\` must run first so the
    /// subsequently-added wildcard escapes don't get themselves
    /// double-escaped.
    pub fn packages_providing_like(
        &self,
        pattern: &str,
        exact_match: &str,
        limit: u32,
    ) -> Result<Vec<(PackageBrief, String)>, RepoError> {
        let guard = self.lock();
        let mut stmt = guard.prepare(
            "SELECT p.name, p.epoch, p.version, p.release, p.arch, \
                    p.size_installed, p.location, c.name \
             FROM packages p JOIN caps c ON c.pkg_id = p.pkg_id \
             WHERE c.kind = 'provides' AND c.name LIKE ?1 ESCAPE '\\' \
             ORDER BY (c.name = ?2) DESC, c.name, p.name LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![pattern, exact_match, limit], |row| {
            let brief = PackageBrief::from_row(row)?;
            let cap_name: String = row.get(7)?;
            Ok((brief, cap_name))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Look up packages providing `name` (via the `provides` cap kind).
    /// Returns pkg_id values.
    pub fn pkg_ids_providing(&self, name: &str) -> Result<Vec<i64>, RepoError> {
        let guard = self.lock();
        let mut stmt = guard.prepare(
            "SELECT DISTINCT pkg_id FROM caps WHERE kind = 'provides' AND name = ?1",
        )?;
        let rows = stmt.query_map(params![name], |row| row.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Owner of an exact path. Returns the first package found (rpm-md
    /// repos guarantee unique ownership; conflicts are surfaced by
    /// separate file-conflict lints).
    pub fn file_owner(&self, path: &str) -> Result<Option<i64>, RepoError> {
        let guard = self.lock();
        Ok(guard
            .query_row(
                "SELECT pkg_id FROM files WHERE path = ?1 LIMIT 1",
                params![path],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// All `(pkg_id, arch)` pairs that own `path`. Multilib repos
    /// commonly ship the same path under both i686 and x86_64 RPMs
    /// (e.g. `/usr/bin/perl` lives in both architectures); the
    /// arch-aware [`RepoUniverse::file_owner`] picks the first
    /// arch-acceptable hit, falling back across the rest.
    pub fn file_owners(&self, path: &str) -> Result<Vec<(i64, Arc<str>)>, RepoError> {
        let guard = self.lock();
        let mut stmt = guard.prepare_cached(
            "SELECT f.pkg_id, p.arch FROM files f \
             JOIN packages p ON p.pkg_id = f.pkg_id \
             WHERE f.path = ?1",
        )?;
        let rows = stmt.query_map(params![path], |row| {
            let pkg_id: i64 = row.get(0)?;
            let arch: String = row.get(1)?;
            Ok((pkg_id, Arc::<str>::from(arch.as_str())))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Reverse-requires: packages that require capability `name`.
    pub fn pkg_ids_requiring(&self, name: &str) -> Result<Vec<i64>, RepoError> {
        let guard = self.lock();
        let mut stmt = guard.prepare(
            "SELECT DISTINCT pkg_id FROM caps WHERE kind = 'requires' AND name = ?1",
        )?;
        let rows = stmt.query_map(params![name], |row| row.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Materialise one full [`Package`] from its pkg_id. Loads NEVRA,
    /// summary, checksum, location, and EVERY capability flavour
    /// (the file list is deliberately skipped — see the body
    /// comment). Returns `Ok(None)` when no row matches the given
    /// `pkg_id` (a stale `ProviderRef` after a re-sync); the
    /// resolver treats that as "skip the candidate", not a fatal
    /// error.
    pub fn load_package(&self, pkg_id: i64) -> Result<Option<Package>, RepoError> {
        let guard = self.lock();
        let header = guard.query_row(
            "SELECT name, epoch, version, release, arch, source_rpm, summary, \
                    size_installed, checksum_alg, checksum_hex, location \
             FROM packages WHERE pkg_id = ?1",
            params![pkg_id],
            |row| {
                let name: String = row.get(0)?;
                let epoch: u32 = row.get(1)?;
                let version: String = row.get(2)?;
                let release: String = row.get(3)?;
                let arch: String = row.get(4)?;
                let source_rpm: Option<String> = row.get(5)?;
                let summary: String = row.get(6)?;
                let size_installed: i64 = row.get(7)?;
                let checksum_alg: String = row.get(8)?;
                let checksum_hex: String = row.get(9)?;
                let location: String = row.get(10)?;
                Ok(PackageHeader {
                    nevra: NEVRA {
                        name: Arc::from(name),
                        epoch,
                        version: Arc::from(version),
                        release: Arc::from(release),
                        arch: Arc::from(arch),
                    },
                    source_rpm: source_rpm.map(Arc::from),
                    summary: Arc::from(summary),
                    size_installed: size_installed as u64,
                    checksum: convert::checksum_from_columns(&checksum_alg, &checksum_hex),
                    location: Arc::from(location),
                })
            },
        ).optional()?;
        let Some(header) = header else {
            return Ok(None);
        };

        let mut provides = Vec::new();
        let mut requires = Vec::new();
        let mut conflicts = Vec::new();
        let mut obsoletes = Vec::new();
        let mut recommends = Vec::new();
        let mut suggests = Vec::new();
        let mut supplements = Vec::new();
        let mut enhances = Vec::new();

        let mut stmt = guard.prepare(
            "SELECT kind, name, flags, epoch, version, release \
             FROM caps WHERE pkg_id = ?1",
        )?;
        let rows = stmt.query_map(params![pkg_id], |row| {
            let kind: String = row.get(0)?;
            let name: String = row.get(1)?;
            let flags: String = row.get(2)?;
            let epoch: Option<u32> = row.get(3)?;
            let version: Option<String> = row.get(4)?;
            let release: Option<String> = row.get(5)?;
            Ok((kind, name, flags, epoch, version, release))
        })?;
        for r in rows {
            let (kind, name, flags, epoch, version, release) = r?;
            let cap = convert::capability_from_columns(
                &name,
                &flags,
                epoch,
                version.as_deref(),
                release.as_deref(),
            )?;
            match kind.as_str() {
                "provides" => provides.push(cap),
                "requires" => requires.push(cap),
                "conflicts" => conflicts.push(cap),
                "obsoletes" => obsoletes.push(cap),
                "recommends" => recommends.push(cap),
                "suggests" => suggests.push(cap),
                "supplements" => supplements.push(cap),
                "enhances" => enhances.push(cap),
                other => {
                    return Err(RepoError::Database(format!(
                        "unknown caps.kind discriminator: {other}"
                    )))
                }
            }
        }
        drop(stmt);

        // `files` is intentionally left empty by this fast path.
        // Loading filelists per package killed `matrix buildroot
        // solve` performance on real specs: the resolver evaluates
        // hundreds of candidates and each `load_package` JOIN'd the
        // ~1 M-row `files` table per call. File-path requirements
        // (`Requires: /usr/bin/foo`) are resolved via the indexed
        // [`Self::owns_file`] query in the resolver, not by
        // scanning `Package.files` in memory — so the only consumer
        // that still wants the full file list calls
        // [`Self::load_package_with_files`] explicitly.
        drop(guard);
        let body = PackageBody {
            provides,
            requires,
            conflicts,
            obsoletes,
            recommends,
            suggests,
            supplements,
            enhances,
            files: Vec::new(),
        };
        let pkg = header.into_package(self.cached_repo_id.clone(), body);
        Ok(Some(pkg))
    }

    /// Like [`Self::load_package`] but also materialises the
    /// per-package filelist. Returns `Ok(None)` when the row is
    /// missing (consistent with `load_package`). Only callers that
    /// genuinely need every file path (rare — `RPM-REPO-011` and
    /// similar scan via `file_owner` instead) should reach for
    /// this; the join over the `files` table is the hot path that
    /// motivated splitting the API in the first place.
    pub fn load_package_with_files(&self, pkg_id: i64) -> Result<Option<Package>, RepoError> {
        let Some(mut pkg) = self.load_package(pkg_id)? else {
            return Ok(None);
        };
        let guard = self.lock();
        let mut stmt = guard.prepare("SELECT path FROM files WHERE pkg_id = ?1")?;
        let rows = stmt.query_map(params![pkg_id], |row| row.get::<_, String>(0))?;
        for r in rows {
            pkg.files.push(Arc::from(r?));
        }
        Ok(Some(pkg))
    }

    /// Indexed "does this package own this exact path?" check.
    /// Hot-path replacement for `Package.files.iter().any(...)` in
    /// the resolver's file-path satisfies check.
    pub fn owns_file(&self, pkg_id: i64, path: &str) -> Result<bool, RepoError> {
        let guard = self.lock();
        Ok(guard
            .query_row(
                "SELECT 1 FROM files WHERE pkg_id = ?1 AND path = ?2 LIMIT 1",
                params![pkg_id, path],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Materialise only the NEVRA-level identity of a pkg_id. Cheap —
    /// no caps / files joins. Returns `Ok(None)` when the row is
    /// missing (stale `ProviderRef`); the resolver treats that as
    /// "skip the candidate", not a fatal error. Used in hot loops
    /// where only the (name, version) tuple is needed.
    pub fn load_nevra(&self, pkg_id: i64) -> Result<Option<NEVRA>, RepoError> {
        let guard = self.lock();
        Ok(guard.query_row(
            "SELECT name, epoch, version, release, arch FROM packages WHERE pkg_id = ?1",
            params![pkg_id],
            |row| {
                let name: String = row.get(0)?;
                let epoch: u32 = row.get(1)?;
                let version: String = row.get(2)?;
                let release: String = row.get(3)?;
                let arch: String = row.get(4)?;
                Ok(NEVRA {
                    name: Arc::from(name),
                    epoch,
                    version: Arc::from(version),
                    release: Arc::from(release),
                    arch: Arc::from(arch),
                })
            },
        ).optional()?)
    }

    /// Fetch every `provides` capability for a pkg_id. Used by the
    /// `provides_satisfies` check (we need to know the version on a
    /// `Provides:` line, not just the package's own NEVRA).
    pub fn load_provides(&self, pkg_id: i64) -> Result<Vec<Capability>, RepoError> {
        let guard = self.lock();
        let mut stmt = guard.prepare(
            "SELECT name, flags, epoch, version, release FROM caps \
             WHERE pkg_id = ?1 AND kind = 'provides'",
        )?;
        let rows = stmt.query_map(params![pkg_id], |row| {
            let name: String = row.get(0)?;
            let flags: String = row.get(1)?;
            let epoch: Option<u32> = row.get(2)?;
            let version: Option<String> = row.get(3)?;
            let release: Option<String> = row.get(4)?;
            Ok((name, flags, epoch, version, release))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (name, flags, epoch, version, release) = r?;
            out.push(convert::capability_from_columns(
                &name,
                &flags,
                epoch,
                version.as_deref(),
                release.as_deref(),
            )?);
        }
        Ok(out)
    }
}

/// Display-oriented summary of one `packages` row. Sized for one
/// rendered line per row in `repo show` output — carries NEVRA plus
/// the two columns operators actually want to see (installed size
/// for sorting, location for download). Cheap to load (no caps /
/// files join).
#[derive(Debug, Clone)]
pub struct PackageBrief {
    pub nevra: NEVRA,
    pub size_installed: u64,
    pub location: Arc<str>,
}

impl PackageBrief {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let name: String = row.get(0)?;
        let epoch: u32 = row.get(1)?;
        let version: String = row.get(2)?;
        let release: String = row.get(3)?;
        let arch: String = row.get(4)?;
        let size_installed: i64 = row.get(5)?;
        let location: String = row.get(6)?;
        Ok(Self {
            nevra: NEVRA {
                name: Arc::from(name),
                epoch,
                version: Arc::from(version),
                release: Arc::from(release),
                arch: Arc::from(arch),
            },
            size_installed: size_installed as u64,
            location: Arc::from(location),
        })
    }
}

/// Heading-only fields of a Package — what's stored in the `packages`
/// row before joining caps + files. Internal type, not exposed.
struct PackageHeader {
    nevra: NEVRA,
    source_rpm: Option<Arc<str>>,
    summary: Arc<str>,
    size_installed: u64,
    checksum: PkgChecksum,
    location: Arc<str>,
}

/// Capability lists + filelist that complement a [`PackageHeader`].
/// Collected by `load_package` from the `provides`/`requires`/... and
/// `files` join tables, then handed to [`PackageHeader::into_package`]
/// as a single bundle so the assembly call doesn't take ten positional
/// arguments. Naming each vector at the call site (vs positional
/// `into_package(provides, requires, conflicts, …)` where every arg
/// has the same `Vec<Capability>` type) catches accidental
/// argument-order swaps during refactors — a structural benefit, not
/// a type-system one.
struct PackageBody {
    provides: Vec<Capability>,
    requires: Vec<Capability>,
    conflicts: Vec<Capability>,
    obsoletes: Vec<Capability>,
    recommends: Vec<Capability>,
    suggests: Vec<Capability>,
    supplements: Vec<Capability>,
    enhances: Vec<Capability>,
    files: Vec<Arc<str>>,
}

impl PackageHeader {
    fn into_package(self, repo_id: RepoId, body: PackageBody) -> Package {
        Package {
            nevra: self.nevra,
            repo_id,
            provides: body.provides,
            requires: body.requires,
            conflicts: body.conflicts,
            obsoletes: body.obsoletes,
            recommends: body.recommends,
            suggests: body.suggests,
            supplements: body.supplements,
            enhances: body.enhances,
            source_rpm: self.source_rpm,
            summary: self.summary,
            size_installed: self.size_installed,
            checksum: self.checksum,
            location: self.location,
            files: body.files,
        }
    }
}

/// Row reader shared by `pkg_briefs_by_name` and
/// `pkg_briefs_providing` — both pull the same six columns
/// (pkg_id + NEVRA tuple).
fn read_pkg_nevra_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, NEVRA)> {
    let pkg_id: i64 = row.get(0)?;
    let name: String = row.get(1)?;
    let epoch: u32 = row.get(2)?;
    let version: String = row.get(3)?;
    let release: String = row.get(4)?;
    let arch: String = row.get(5)?;
    Ok((
        pkg_id,
        NEVRA {
            name: Arc::from(name),
            epoch,
            version: Arc::from(version),
            release: Arc::from(release),
            arch: Arc::from(arch),
        },
    ))
}


/// Pragmas for the bulk-write path used by `repo sync`. The DB
/// is created at a `.tmp` location and atomic-renamed into place
/// only after `ingest_packages` succeeds, so crash-safety is
/// "either we rename or we don't" — neither WAL nor `fsync` is
/// load-bearing here.
///
/// * `page_size = 16384` — set BEFORE any table is created so the
///   on-disk file uses 16 KiB pages instead of the 4 KiB default;
///   each page-cache spill `pwrite` carries 4× more rows.
/// * `journal_mode = OFF` + `synchronous = OFF` — no rollback
///   journal, no per-flush `fsync`. The `.tmp → rename` model
///   guarantees atomicity at the filesystem level.
/// * `cache_size = -524288` (512 MiB) + `temp_store = MEMORY` —
///   the entire 60 k-package ingest stays resident, avoiding
///   the spill-to-disk pattern the strace was showing.
///
/// Live readers re-open via [`apply_read_pragmas`] which restores
/// WAL + `synchronous = NORMAL` with a smaller (64 MiB) cache.
fn apply_ingest_pragmas(conn: &Connection) -> Result<(), RepoError> {
    conn.execute_batch(
        "PRAGMA page_size = 16384;\
         PRAGMA journal_mode = OFF;\
         PRAGMA synchronous = OFF;\
         PRAGMA cache_size = -524288;\
         PRAGMA temp_store = MEMORY;",
    )?;
    tracing::debug!(
        mode = "ingest",
        page_size = 16384,
        cache_mib = 512,
        journal_mode = "OFF",
        synchronous = "OFF",
        "applied SQLite ingest pragmas",
    );
    Ok(())
}

fn apply_read_pragmas(conn: &Connection) -> Result<(), RepoError> {
    // WAL keeps writers and readers from blocking each other; we have
    // exactly one writer (`repo sync`) and many readers (every lint
    // pass). NORMAL sync trades durability for write speed — a fresh
    // sync can re-populate from XML, so we don't need the cost of
    // FULL fsyncs. cache_size is in -KiB (negative = KiB units) per
    // SQLite convention; 64 MiB keeps the hot index pages resident
    // without pinning the whole index file.
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;\
         PRAGMA synchronous = NORMAL;\
         PRAGMA cache_size = -65536;\
         PRAGMA temp_store = MEMORY;",
    )?;
    tracing::debug!(
        mode = "read",
        page_size = "file-header",
        cache_mib = 64,
        journal_mode = "WAL",
        synchronous = "NORMAL",
        "applied SQLite read pragmas",
    );
    Ok(())
}

fn insert_cap_batch(
    stmt: &mut rusqlite::Statement<'_>,
    pkg_id: i64,
    kind: CapKind,
    caps: &[Capability],
) -> Result<(), RepoError> {
    for cap in caps {
        let (name, flags, epoch, version, release) = convert::capability_columns(cap);
        stmt.execute(params![
            pkg_id,
            kind.as_str(),
            name,
            flags,
            epoch,
            version,
            release,
        ])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::PkgChecksum;

    fn sample_pkg(name: &str, version: &str) -> Package {
        Package {
            nevra: NEVRA {
                name: Arc::from(name),
                epoch: 0,
                version: Arc::from(version),
                release: Arc::from("1.el9"),
                arch: Arc::from("x86_64"),
            },
            repo_id: RepoId::from("test"),
            provides: vec![Capability::unversioned(Arc::from(name))],
            requires: vec![Capability::ge(
                "glibc",
                crate::EVR::new(Some(0), "2.28", "0"),
            )],
            conflicts: Vec::new(),
            obsoletes: Vec::new(),
            recommends: Vec::new(),
            suggests: Vec::new(),
            supplements: Vec::new(),
            enhances: Vec::new(),
            source_rpm: Some(Arc::from(format!("{name}-{version}-1.el9.src.rpm"))),
            summary: Arc::from(""),
            size_installed: 12345,
            checksum: PkgChecksum::Sha256("deadbeef".to_string()),
            location: Arc::from(format!("Packages/{name}-{version}.rpm")),
            files: vec![Arc::from(format!("/usr/bin/{name}"))],
        }
    }

    #[test]
    fn create_open_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("repo.db");
        let now = OffsetDateTime::now_utc();
        let mut db = RepoDb::create(
            &path,
            &RepoId::from("baseos"),
            "deadbeef",
            now,
            "rpm-md",
            "abc123",
        )
        .unwrap();
        let inserted = db.ingest_packages(&[sample_pkg("bash", "5.1.8")]).unwrap();
        assert_eq!(inserted, 1);
        drop(db);

        let db2 = RepoDb::open(&path).unwrap();
        assert_eq!(db2.repo_id().unwrap().as_str(), "baseos");
        assert_eq!(db2.revision().unwrap(), "deadbeef");
        assert_eq!(db2.package_count().unwrap(), 1);
        let ids = db2.pkg_ids_by_name("bash").unwrap();
        assert_eq!(ids.len(), 1);
        let pkg = db2.load_package(ids[0]).unwrap().expect("pkg row present");
        assert_eq!(pkg.nevra.name.as_ref(), "bash");
        assert_eq!(pkg.requires.len(), 1);
        // `load_package` no longer materialises the filelist — that
        // join was the killer on `matrix buildroot solve`. Use the
        // explicit `load_package_with_files` variant for callers
        // that actually need every path.
        assert!(pkg.files.is_empty(), "load_package must skip files");
        let pkg_full = db2
            .load_package_with_files(ids[0])
            .unwrap()
            .expect("pkg row present");
        assert_eq!(pkg_full.files.len(), 1);
        // Stale ProviderRef must yield Ok(None), not abort the run.
        assert!(db2.load_package(9_999_999).unwrap().is_none());
        assert!(db2.load_nevra(9_999_999).unwrap().is_none());
        // Hot-path file ownership check.
        assert!(db2.owns_file(ids[0], "/usr/bin/bash").unwrap());
        assert!(!db2.owns_file(ids[0], "/nowhere").unwrap());
    }

    #[test]
    fn provides_lookup_returns_pkg_id() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("repo.db");
        let mut db = RepoDb::create(
            &path,
            &RepoId::from("baseos"),
            "rev",
            OffsetDateTime::now_utc(),
            "rpm-md",
            "sha",
        )
        .unwrap();
        let mut pkg = sample_pkg("systemd-devel", "252");
        pkg.provides.push(Capability::unversioned(Arc::from("pkgconfig(libsystemd)")));
        db.ingest_packages(&[pkg]).unwrap();

        let providers = db.pkg_ids_providing("pkgconfig(libsystemd)").unwrap();
        assert_eq!(providers.len(), 1);
    }

    #[test]
    fn file_owner_lookup() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("repo.db");
        let mut db = RepoDb::create(
            &path,
            &RepoId::from("baseos"),
            "rev",
            OffsetDateTime::now_utc(),
            "rpm-md",
            "sha",
        )
        .unwrap();
        db.ingest_packages(&[sample_pkg("bash", "5.1.8")]).unwrap();
        let owner = db.file_owner("/usr/bin/bash").unwrap();
        assert!(owner.is_some());
        assert!(db.file_owner("/nowhere").unwrap().is_none());
    }

    #[test]
    fn schema_mismatch_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("repo.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE meta (key TEXT, value TEXT); \
                 INSERT INTO meta VALUES ('schema_version', '99');",
            )
            .unwrap();
        }
        let res = RepoDb::open(&path);
        assert!(matches!(res, Err(RepoError::Database(_))));
    }

    // ---------------------------------------------------------------
    // package_satisfies / owns_file direct unit tests.
    //
    // These exercise the resolver's hot-path satisfies check without
    // routing through `repo-resolver::pick_provider`. The helper
    // builds an in-memory DB so each test stays self-contained — no
    // tmpdir, no filesystem.
    // ---------------------------------------------------------------

    /// Build a `Package` with an explicit `Provides:` list. Other
    /// capability vectors are left empty.
    fn pkg_with_provides(name: &str, version: &str, provides: Vec<Capability>) -> Package {
        Package {
            nevra: NEVRA {
                name: Arc::from(name),
                epoch: 0,
                version: Arc::from(version),
                release: Arc::from("1.el9"),
                arch: Arc::from("x86_64"),
            },
            repo_id: RepoId::from("test"),
            provides,
            requires: Vec::new(),
            conflicts: Vec::new(),
            obsoletes: Vec::new(),
            recommends: Vec::new(),
            suggests: Vec::new(),
            supplements: Vec::new(),
            enhances: Vec::new(),
            source_rpm: None,
            summary: Arc::from(""),
            size_installed: 0,
            checksum: PkgChecksum::Sha256(String::new()),
            location: Arc::from(format!("Packages/{name}-{version}.rpm")),
            files: Vec::new(),
        }
    }

    /// In-memory DB seeded with the supplied packages. Returns the
    /// DB plus the `pkg_id` of each package in the same input order.
    fn satisfies_fixture(packages: Vec<Package>) -> (RepoDb, Vec<i64>) {
        let mut db = RepoDb::create_in_memory(
            &RepoId::from("test"),
            "rev",
            OffsetDateTime::now_utc(),
            "rpm-md",
            "sha",
        )
        .unwrap();
        let names: Vec<String> = packages.iter().map(|p| p.nevra.name.to_string()).collect();
        db.ingest_packages(&packages).unwrap();
        // Resolve pkg_ids in input order (one package per name in
        // these fixtures, so `pkg_ids_by_name(name)[0]` is unique).
        let ids = names
            .iter()
            .map(|n| db.pkg_ids_by_name(n).unwrap()[0])
            .collect();
        (db, ids)
    }

    #[test]
    fn package_satisfies_via_name_no_version() {
        let (db, ids) = satisfies_fixture(vec![pkg_with_provides("bash", "5.1.8", vec![])]);
        let req = Capability::unversioned(Arc::from("bash"));
        assert!(db.package_satisfies(ids[0], &req).unwrap());
    }

    #[test]
    fn package_satisfies_via_name_ge_pass() {
        let (db, ids) = satisfies_fixture(vec![pkg_with_provides("bash", "5.1.8", vec![])]);
        let req = Capability::ge("bash", crate::EVR::new(Some(0), "5.0.0", "1.el9"));
        assert!(db.package_satisfies(ids[0], &req).unwrap());
    }

    #[test]
    fn package_satisfies_via_name_ge_fail() {
        let (db, ids) = satisfies_fixture(vec![pkg_with_provides("bash", "5.1.8", vec![])]);
        let req = Capability::ge("bash", crate::EVR::new(Some(0), "6.0.0", "1.el9"));
        assert!(!db.package_satisfies(ids[0], &req).unwrap());
    }

    #[test]
    fn package_satisfies_via_provides_unversioned() {
        // Package's own name is `systemd-devel`; request the virtual
        // capability `pkgconfig(libsystemd)` (no version constraint)
        // → must match via the Provides: row.
        let (db, ids) = satisfies_fixture(vec![pkg_with_provides(
            "systemd-devel",
            "252",
            vec![Capability::unversioned(Arc::from("pkgconfig(libsystemd)"))],
        )]);
        let req = Capability::unversioned(Arc::from("pkgconfig(libsystemd)"));
        assert!(db.package_satisfies(ids[0], &req).unwrap());
    }

    #[test]
    fn package_satisfies_via_provides_versioned() {
        // Provides row carries an EVR (`Provides: foo = 1.2-3`). A
        // versioned GE request whose EVR is satisfied by the row
        // must return true even though the package's own name
        // differs from the request.
        let (db, ids) = satisfies_fixture(vec![pkg_with_provides(
            "libfoo",
            "1.2",
            vec![Capability::eq("foo", crate::EVR::new(Some(0), "1.2", "3"))],
        )]);
        let req = Capability::ge("foo", crate::EVR::new(Some(0), "1.0", "1"));
        assert!(db.package_satisfies(ids[0], &req).unwrap());
    }

    #[test]
    fn package_satisfies_via_provides_versioned_fails_when_unversioned() {
        // Provides row is unversioned (`Provides: foo`); a versioned
        // GE request cannot be satisfied — `package_satisfies` skips
        // rows where (version, release) are NULL.
        let (db, ids) = satisfies_fixture(vec![pkg_with_provides(
            "libfoo",
            "1.2",
            vec![Capability::unversioned(Arc::from("foo"))],
        )]);
        let req = Capability::ge("foo", crate::EVR::new(Some(0), "1.0", "1"));
        assert!(!db.package_satisfies(ids[0], &req).unwrap());
    }

    #[test]
    fn package_satisfies_eq_exact() {
        let (db, ids) = satisfies_fixture(vec![pkg_with_provides("bash", "5.1.8", vec![])]);
        let req = Capability::eq("bash", crate::EVR::new(Some(0), "5.1.8", "1.el9"));
        assert!(db.package_satisfies(ids[0], &req).unwrap());
    }

    #[test]
    fn package_satisfies_negative() {
        // Package name = "bash", no virtual provides. A request for
        // "glibc" must not be satisfied.
        let (db, ids) = satisfies_fixture(vec![pkg_with_provides("bash", "5.1.8", vec![])]);
        let req = Capability::unversioned(Arc::from("glibc"));
        assert!(!db.package_satisfies(ids[0], &req).unwrap());
    }

    #[test]
    fn owns_file_is_per_package() {
        // Two packages, each owning one distinct file. `owns_file`
        // must return true only for the (pkg_id, path) pair that
        // actually matches — not for the other package's path.
        let mut a = pkg_with_provides("alpha", "1.0", vec![]);
        a.files = vec![Arc::from("/usr/bin/alpha")];
        let mut b = pkg_with_provides("beta", "1.0", vec![]);
        b.files = vec![Arc::from("/usr/bin/beta")];
        let (db, ids) = satisfies_fixture(vec![a, b]);
        let (alpha_id, beta_id) = (ids[0], ids[1]);

        assert!(db.owns_file(alpha_id, "/usr/bin/alpha").unwrap());
        assert!(db.owns_file(beta_id, "/usr/bin/beta").unwrap());

        // Cross-package: the file belongs to the *other* package.
        assert!(!db.owns_file(alpha_id, "/usr/bin/beta").unwrap());
        assert!(!db.owns_file(beta_id, "/usr/bin/alpha").unwrap());

        // Non-existent path on a real package.
        assert!(!db.owns_file(alpha_id, "/nowhere").unwrap());
    }
}
