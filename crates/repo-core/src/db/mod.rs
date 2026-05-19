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
        apply_pragmas(&conn)?;
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
        stmt.execute(params![schema::meta_keys::REPO_ID, repo_id.as_ref()])?;
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
        apply_pragmas(&conn)?;
        conn.execute_batch(schema::CREATE_SQL)?;
        let fetched_at_str = fetched_at
            .format(&Rfc3339)
            .map_err(|e| RepoError::Database(format!("rfc3339 format: {e}")))?;
        let mut stmt = conn.prepare("INSERT INTO meta (key, value) VALUES (?1, ?2)")?;
        stmt.execute(params![
            schema::meta_keys::SCHEMA_VERSION,
            schema::SCHEMA_VERSION.to_string()
        ])?;
        stmt.execute(params![schema::meta_keys::REPO_ID, repo_id.as_ref()])?;
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
        apply_pragmas(&conn)?;

        let version: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![schema::meta_keys::SCHEMA_VERSION],
                |row| row.get(0),
            )
            .optional()?;

        let cached_repo_id: RepoId = match conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![schema::meta_keys::REPO_ID],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            Some(s) => Arc::from(s),
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

    /// Acquire the inner connection. Panics on poison, which is an
    /// unrecoverable corruption signal.
    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("repo db mutex poisoned")
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
                 (name, epoch, version, release, arch, source_rpm, summary, \
                  size_installed, checksum_alg, checksum_hex, location) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?;
            let mut insert_cap = tx.prepare(
                "INSERT INTO caps \
                 (pkg_id, kind, name, flags, epoch, version, release) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            let mut insert_file = tx.prepare("INSERT INTO files (pkg_id, path) VALUES (?1, ?2)")?;

            for pkg in packages {
                let (alg, hex) = convert::checksum_columns(&pkg.checksum);
                insert_pkg.execute(params![
                    pkg.nevra.name.as_ref(),
                    pkg.nevra.epoch,
                    pkg.nevra.version.as_ref(),
                    pkg.nevra.release.as_ref(),
                    pkg.nevra.arch.as_ref(),
                    pkg.source_rpm.as_deref(),
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
        let mut stmt = guard.prepare("SELECT pkg_id FROM packages WHERE name = ?1")?;
        let rows = stmt.query_map(params![name], |row| row.get::<_, i64>(0))?;
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
    /// summary, checksum, location, and EVERY capability flavour plus
    /// the file list. Use only when the consumer actually needs all
    /// of that (the resolver only needs Provides + name; bulk loads
    /// of every package defeat the purpose of the SQLite backend).
    pub fn load_package(&self, pkg_id: i64) -> Result<Package, RepoError> {
        let guard = self.lock();
        let pkg = guard.query_row(
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
        )?;

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

        let mut files = Vec::new();
        let mut stmt = guard.prepare("SELECT path FROM files WHERE pkg_id = ?1")?;
        let rows = stmt.query_map(params![pkg_id], |row| row.get::<_, String>(0))?;
        for r in rows {
            files.push(Arc::from(r?));
        }
        drop(stmt);

        // Drop the guard before touching `cached_repo_id` (which
        // doesn't require the lock anyway, but staying explicit
        // makes the lock scope obvious).
        drop(guard);
        let pkg = pkg.into_package(
            self.cached_repo_id.clone(),
            provides,
            requires,
            conflicts,
            obsoletes,
            recommends,
            suggests,
            supplements,
            enhances,
            files,
        );
        Ok(pkg)
    }

    /// Materialise only the NEVRA-level identity of a pkg_id. Cheap —
    /// no caps / files joins. Used by the resolver to build the
    /// [`crate::ProviderRef`] target and report the chosen NEVRA in
    /// diagnostics without paying the full package load.
    pub fn load_nevra(&self, pkg_id: i64) -> Result<NEVRA, RepoError> {
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
        )?)
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

#[allow(clippy::too_many_arguments)]
impl PackageHeader {
    fn into_package(
        self,
        repo_id: RepoId,
        provides: Vec<Capability>,
        requires: Vec<Capability>,
        conflicts: Vec<Capability>,
        obsoletes: Vec<Capability>,
        recommends: Vec<Capability>,
        suggests: Vec<Capability>,
        supplements: Vec<Capability>,
        enhances: Vec<Capability>,
        files: Vec<Arc<str>>,
    ) -> Package {
        Package {
            nevra: self.nevra,
            repo_id,
            provides,
            requires,
            conflicts,
            obsoletes,
            recommends,
            suggests,
            supplements,
            enhances,
            source_rpm: self.source_rpm,
            summary: self.summary,
            size_installed: self.size_installed,
            checksum: self.checksum,
            location: self.location,
            files,
        }
    }
}

fn apply_pragmas(conn: &Connection) -> Result<(), RepoError> {
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
    use crate::package::{CapFlags, PkgChecksum};

    fn sample_pkg(name: &str, version: &str) -> Package {
        Package {
            nevra: NEVRA {
                name: Arc::from(name),
                epoch: 0,
                version: Arc::from(version),
                release: Arc::from("1.el9"),
                arch: Arc::from("x86_64"),
            },
            repo_id: Arc::from("test"),
            provides: vec![Capability {
                name: Arc::from(name),
                flags: CapFlags::None,
                evr: None,
            }],
            requires: vec![Capability {
                name: Arc::from("glibc"),
                flags: CapFlags::GE,
                evr: Some(crate::EVR::new(Some(0), "2.28", "0")),
            }],
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
            &Arc::from("baseos"),
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
        assert_eq!(db2.repo_id().unwrap().as_ref(), "baseos");
        assert_eq!(db2.revision().unwrap(), "deadbeef");
        assert_eq!(db2.package_count().unwrap(), 1);
        let ids = db2.pkg_ids_by_name("bash").unwrap();
        assert_eq!(ids.len(), 1);
        let pkg = db2.load_package(ids[0]).unwrap();
        assert_eq!(pkg.nevra.name.as_ref(), "bash");
        assert_eq!(pkg.requires.len(), 1);
        assert_eq!(pkg.files.len(), 1);
    }

    #[test]
    fn provides_lookup_returns_pkg_id() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("repo.db");
        let mut db = RepoDb::create(
            &path,
            &Arc::from("baseos"),
            "rev",
            OffsetDateTime::now_utc(),
            "rpm-md",
            "sha",
        )
        .unwrap();
        let mut pkg = sample_pkg("systemd-devel", "252");
        pkg.provides.push(Capability {
            name: Arc::from("pkgconfig(libsystemd)"),
            flags: CapFlags::None,
            evr: None,
        });
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
            &Arc::from("baseos"),
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
}
