//! Per-repo indexes and the assembled per-profile universe.
//!
//! [`RepoIndex`] is the transient parser output (a `Vec<Package>` plus
//! metadata) — it lives only between XML parsing and DB ingestion.
//! Production lookups go through [`RepoUniverse`], which is backed by
//! one [`crate::db::RepoDb`] per repository.
//!
//! ## Why DB-backed instead of in-RAM
//!
//! A real distribution profile (BaseOS + AppStream + Updates + a
//! vendor repo) carries ~50–80k packages with millions of file paths.
//! The previous in-RAM model peaked at 5+ GB resident on a single
//! `matrix deps check` invocation because bincode loaded every
//! capability + every file path as an independent allocation (no
//! `Arc<str>` dedup). The SQLite backend trades a fixed page-cache
//! budget (~64 MiB) for query latency that is still microseconds on
//! the hot path because the relevant indexes stay resident.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::db::RepoDb;
use crate::error::RepoError;
use crate::package::{NEVRA, Package};

/// Stable identifier carried through indexes and provider references
/// so diagnostics can attribute a finding to one specific repo.
///
/// Conventionally the TOML key under `[profiles.X.repos.<id>]` —
/// the backend is expected to receive it from the caller and stuff
/// it into the produced [`RepoIndex`]. Backends that don't have the
/// caller's id available fall back to the canonicalised baseurl;
/// callers should not parse this string.
pub type RepoId = Arc<str>;

/// Identity of one fetched metadata snapshot. `id` is the sha256 of
/// the format-defining file (`repomd.xml` for rpm-md, `base/release`
/// for apt-rpm) and is what the lockfile records.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoRevision {
    pub id: String,
    pub timestamp: OffsetDateTime,
    /// Raw bytes of the format-defining file, retained for downstream
    /// re-hashing without a fresh fetch.
    pub raw_bytes: Vec<u8>,
}

/// One parsed snapshot of one repository. **Transient.**
///
/// Backends in `rpm-spec-repo-metadata` populate this struct directly
/// from XML / pkglist parsing. The cache layer then writes it into
/// a `repo.db` via [`crate::db::RepoDb::ingest_packages`] and drops
/// the in-memory copy. After Phase 5 of the SQLite migration the
/// bincode round-trip (`serialize` / `deserialize`) goes away
/// entirely; the serde derives stay only to keep test fixtures
/// trivially constructible and to keep the cache writer's signature
/// stable while both code paths coexist.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoIndex {
    pub repo_id: RepoId,
    pub revision: String,
    pub fetched_at: OffsetDateTime,
    pub packages: Vec<Package>,
    /// Optional updateinfo advisories. Empty for apt-rpm (no
    /// equivalent format) and for rpm-md repos that don't ship one.
    pub advisories: Vec<Advisory>,
}

impl RepoIndex {
    /// Convenience: empty index with the given identity. Used by
    /// fixture tests.
    #[must_use]
    pub fn empty(repo_id: RepoId, revision: impl Into<String>, fetched_at: OffsetDateTime) -> Self {
        Self {
            repo_id,
            revision: revision.into(),
            fetched_at,
            packages: Vec::new(),
            advisories: Vec::new(),
        }
    }
}

/// Cross-repo reference to one specific package row inside the DB
/// behind a [`RepoUniverse`]. The repo id locates the right database
/// and `pkg_id` is the row's `INTEGER PRIMARY KEY`. Cheap to clone
/// (one Arc bump + an i64 copy).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct ProviderRef {
    pub repo_id: RepoId,
    pub pkg_id: i64,
}

impl ProviderRef {
    /// Materialise the referenced package by querying the universe's
    /// backing database. Returns `Ok(None)` if the universe doesn't
    /// contain the named repo (caller bug) or the row is missing
    /// (data corruption).
    ///
    /// Each call hits SQLite — avoid in tight resolver loops where
    /// only the NEVRA is needed; use [`RepoUniverse::resolve_nevra`]
    /// for the cheap path.
    pub fn resolve(&self, universe: &RepoUniverse) -> Result<Option<Package>, RepoError> {
        universe.resolve(self)
    }
}

/// Assembled per-profile package universe.
///
/// Holds one open [`RepoDb`] per repository, in priority order (lowest
/// index = highest priority). All lookup tables are fronted by query
/// methods rather than precomputed HashMaps — SQLite indexes back the
/// equivalent of the old `provides_by_name` / `by_name` etc. fields,
/// at a fraction of the RAM cost.
#[derive(Debug)]
#[non_exhaustive]
pub struct RepoUniverse {
    pub profile_name: String,
    /// DBs in priority order (ascending — lowest index = highest priority).
    pub dbs: Vec<RepoDb>,
    /// Snapshot ids per repo — recorded into the lockfile when the
    /// universe is locked.
    pub snapshot_ids: BTreeMap<RepoId, String>,
    /// Index from repo id to position in `dbs`. Built once by the
    /// constructor so `ProviderRef::resolve` is O(1) on the repo
    /// dispatch step.
    repo_by_id: HashMap<RepoId, usize>,
}

impl RepoUniverse {
    /// Build a universe from a list of opened DBs. Caller orders by
    /// priority. Reads the `meta` rows of each DB once to populate
    /// the snapshot id map.
    pub fn from_dbs(
        profile_name: impl Into<String>,
        dbs: Vec<RepoDb>,
    ) -> Result<Self, RepoError> {
        let mut snapshot_ids: BTreeMap<RepoId, String> = BTreeMap::new();
        let mut repo_by_id: HashMap<RepoId, usize> = HashMap::new();
        for (i, db) in dbs.iter().enumerate() {
            let id = db.repo_id()?;
            let rev = db.revision()?;
            snapshot_ids.insert(id.clone(), rev);
            repo_by_id.insert(id, i);
        }
        Ok(Self {
            profile_name: profile_name.into(),
            dbs,
            snapshot_ids,
            repo_by_id,
        })
    }

    /// Test-only helper: build a universe from in-memory
    /// [`RepoIndex`] values by ingesting each into a fresh
    /// `:memory:` SQLite database. Used by every test fixture in
    /// the analyzer + resolver crates that wants to hand-craft a
    /// tiny universe without touching the filesystem.
    ///
    /// Production code paths must use [`Self::from_dbs`] with
    /// disk-backed [`RepoDb`]s (the cache layer does the open).
    pub fn from_indexes_for_tests(
        profile_name: impl Into<String>,
        indexes: Vec<Arc<RepoIndex>>,
    ) -> Result<Self, RepoError> {
        let mut dbs = Vec::with_capacity(indexes.len());
        for idx in indexes {
            let mut db = RepoDb::create_in_memory(
                &idx.repo_id,
                &idx.revision,
                idx.fetched_at,
                "rpm-md",
                "test-fixture",
            )?;
            db.ingest_packages(&idx.packages)?;
            dbs.push(db);
        }
        Self::from_dbs(profile_name, dbs)
    }

    /// Total package count across all repos. Issues one COUNT per
    /// repo (cheap thanks to the rowid index).
    pub fn package_count(&self) -> Result<u64, RepoError> {
        let mut n = 0;
        for db in &self.dbs {
            n += db.package_count()?;
        }
        Ok(n)
    }

    /// All packages whose own NAME matches `name`. Used by the
    /// resolver when a `Requires: foo` names a package directly.
    pub fn by_name(&self, name: &str) -> Result<Vec<ProviderRef>, RepoError> {
        let mut out = Vec::new();
        for db in &self.dbs {
            let repo_id = db.repo_id()?;
            for pkg_id in db.pkg_ids_by_name(name)? {
                out.push(ProviderRef {
                    repo_id: repo_id.clone(),
                    pkg_id,
                });
            }
        }
        Ok(out)
    }

    /// All packages whose `Provides:` includes `name` (virtual
    /// capabilities like `pkgconfig(foo)`).
    pub fn provides_by_name(&self, name: &str) -> Result<Vec<ProviderRef>, RepoError> {
        let mut out = Vec::new();
        for db in &self.dbs {
            let repo_id = db.repo_id()?;
            for pkg_id in db.pkg_ids_providing(name)? {
                out.push(ProviderRef {
                    repo_id: repo_id.clone(),
                    pkg_id,
                });
            }
        }
        Ok(out)
    }

    /// Owner of a path. Returns `Ok(None)` if no repo claims the
    /// file. Priority-aware: the first (lowest-priority-index) repo
    /// to report ownership wins, matching dnf's resolution.
    pub fn file_owner(&self, path: &str) -> Result<Option<ProviderRef>, RepoError> {
        for db in &self.dbs {
            if let Some(pkg_id) = db.file_owner(path)? {
                return Ok(Some(ProviderRef {
                    repo_id: db.repo_id()?,
                    pkg_id,
                }));
            }
        }
        Ok(None)
    }

    /// `by_name` plus `provides_by_name` in a single sweep, with
    /// each candidate paired with its NEVRA. Saves the resolver
    /// from issuing one `resolve_nevra` per candidate just to feed
    /// the tie-break ordering. Insertion order is preserved
    /// (by-name first, then by-provides), dedup is by
    /// `ProviderRef`.
    pub fn candidates_with_nevra(
        &self,
        name: &str,
    ) -> Result<Vec<(ProviderRef, NEVRA)>, RepoError> {
        use std::collections::HashSet;
        let mut seen: HashSet<ProviderRef> = HashSet::new();
        let mut out: Vec<(ProviderRef, NEVRA)> = Vec::new();
        for db in &self.dbs {
            let repo_id = db.repo_id()?;
            for (pkg_id, nevra) in db.pkg_briefs_by_name(name)? {
                let pref = ProviderRef {
                    repo_id: repo_id.clone(),
                    pkg_id,
                };
                if seen.insert(pref.clone()) {
                    out.push((pref, nevra));
                }
            }
        }
        for db in &self.dbs {
            let repo_id = db.repo_id()?;
            for (pkg_id, nevra) in db.pkg_briefs_providing(name)? {
                let pref = ProviderRef {
                    repo_id: repo_id.clone(),
                    pkg_id,
                };
                if seen.insert(pref.clone()) {
                    out.push((pref, nevra));
                }
            }
        }
        Ok(out)
    }

    /// Hot-path "does `pref` satisfy `req`?" check. Routes to
    /// [`crate::db::RepoDb::package_satisfies`] without
    /// materialising the full `Package`.
    ///
    /// **File-path deps:** for `req.name` starting with `/`, callers must
    /// use [`Self::file_owner`] instead — `satisfies` doesn't query the
    /// `files` table and will return `false` for file-path requirements
    /// even when the package owns the path.
    pub fn satisfies(
        &self,
        pref: &ProviderRef,
        req: &crate::package::Capability,
    ) -> Result<bool, RepoError> {
        let Some(idx) = self.repo_by_id.get(&pref.repo_id).copied() else {
            return Ok(false);
        };
        self.dbs[idx].package_satisfies(pref.pkg_id, req)
    }

    /// Reverse-requires lookup: packages that require capability
    /// `name`. Used by P1 reverse-dep impact lints.
    pub fn reverse_requires(&self, name: &str) -> Result<Vec<ProviderRef>, RepoError> {
        let mut out = Vec::new();
        for db in &self.dbs {
            let repo_id = db.repo_id()?;
            for pkg_id in db.pkg_ids_requiring(name)? {
                out.push(ProviderRef {
                    repo_id: repo_id.clone(),
                    pkg_id,
                });
            }
        }
        Ok(out)
    }

    /// Materialise the full [`Package`] referenced by `pref`. Issues
    /// one SELECT per join (package header + caps + files). Returns
    /// `Ok(None)` if the repo id isn't in this universe or the row
    /// is missing — both are silent-skip conditions for callers that
    /// merely want to iterate candidates.
    pub fn resolve(&self, pref: &ProviderRef) -> Result<Option<Package>, RepoError> {
        let Some(idx) = self.repo_by_id.get(&pref.repo_id).copied() else {
            return Ok(None);
        };
        // `load_package` now returns `Ok(None)` for a missing row
        // (was previously mapped via `match Err(RepoError::Database)`
        // — Cycle A's `From<rusqlite::Error>` rerouted those to
        // `Sqlite`, silently breaking the matcher and turning a
        // stale `ProviderRef` into a hard abort instead of a skip).
        self.dbs[idx].load_package(pref.pkg_id)
    }

    /// Cheap NEVRA-only resolve. Suitable for hot loops where the
    /// resolver only needs the (name, version) tuple to construct a
    /// `Candidate` (avoids loading caps + files for every candidate).
    pub fn resolve_nevra(&self, pref: &ProviderRef) -> Result<Option<NEVRA>, RepoError> {
        let Some(idx) = self.repo_by_id.get(&pref.repo_id).copied() else {
            return Ok(None);
        };
        self.dbs[idx].load_nevra(pref.pkg_id)
    }

    /// Locate the DB for `repo_id` (priority-index lookup). Used by
    /// helpers that need direct DB access (e.g. to load `Provides:`
    /// rows for the satisfies check).
    pub fn db_for(&self, repo_id: &RepoId) -> Option<&RepoDb> {
        let idx = self.repo_by_id.get(repo_id).copied()?;
        self.dbs.get(idx)
    }

    /// Repo priority index — lower is better. Returns `None` for
    /// repos not present in this universe (typically a stale
    /// reference). Replaces the standalone `build_priority_map`
    /// helper.
    pub fn priority_of(&self, repo_id: &RepoId) -> Option<i32> {
        let i = self.repo_by_id.get(repo_id).copied()?;
        i32::try_from(i).ok()
    }
}

/// One updateinfo advisory. Populated from `updateinfo.xml` (rpm-md
/// only). Used by `RPM-REPO-090/091` in M5.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Advisory {
    pub id: String,
    pub severity: AdvisorySeverity,
    pub fixed_packages: Vec<NEVRA>,
    pub cves: Vec<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum AdvisorySeverity {
    Low,
    Moderate,
    Important,
    Critical,
    Unknown,
}
