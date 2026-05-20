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
/// Always the TOML key under `[profiles.X.repos.<id>]` (a short slug
/// like `baseos`). The format is `[a-z0-9_-]{1,64}` — enforced by
/// `rpm_spec_profile::repos::validate_repo_id`, called at TOML load
/// time by `rpm_spec_profile::resolve::apply_repos`. Both production
/// CLI paths (`cli::commands::repo::sync`,
/// `cli::commands::matrix::universe`) feed this slug verbatim into
/// [`Self::from`], so the validation invariant propagates to every
/// `RepoIndex.repo_id` produced by a backend in normal use.
///
/// Newtype around `Arc<str>` so the compiler distinguishes a repo
/// identifier from arbitrary string-shaped data in function signatures
/// (e.g. you can't accidentally pass a package name where a repo id is
/// expected). The wrapping is zero-cost; cloning is one `Arc` refcount
/// bump (O(1)).
///
/// Construction is intentionally infallible — `From<&str>` /
/// `From<String>` cover the internal hot paths. Construction does
/// NOT re-run `validate_repo_id`; the contract is that callers above
/// `RepoId` have already validated. The DB open path
/// ([`crate::db::RepoDb::open`]) is the one cross-trust boundary, and
/// it re-validates the cached `meta.repo_id` byte for byte so a
/// tampered cache can't smuggle ANSI escapes / NULs / oversized
/// strings into log lines and diagnostics.
///
/// # Examples
///
/// ```
/// use rpm_spec_repo_core::RepoId;
/// let id = RepoId::from("baseos");
/// assert_eq!(id.as_str(), "baseos");
/// ```
#[derive(Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RepoId(Arc<str>);

// Static `Send + Sync` guard — `RepoId` is keyed by `HashMap` /
// `BTreeMap` inside a long-lived `Arc<RepoUniverse>` shared across
// analyzer threads. A future inner-type change (`Rc`, `Cell`, etc.)
// would silently break that sharing; this assert turns it into a
// compile error.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<RepoId>();
};

// Manual `Debug` so `?repo_id` in tracing fields keeps producing
// `"baseos"` (the pre-newtype representation) rather than
// `RepoId("baseos")` — observability consumers parse this shape.
impl std::fmt::Debug for RepoId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&*self.0, f)
    }
}

impl RepoId {
    /// Borrow the underlying string slice. Equivalent to
    /// `<Self as AsRef<str>>::as_ref(self)`, but avoids the
    /// `AsRef<str>` / `Borrow<str>` disambiguation at non-format call
    /// sites (SQL params, function args). Format-string call sites
    /// should use `{repo_id}` (Display) or `{repo_id:?}` (Debug).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for RepoId {
    fn from(s: &str) -> Self {
        Self(Arc::from(s))
    }
}

impl From<String> for RepoId {
    fn from(s: String) -> Self {
        Self(Arc::from(s))
    }
}

impl AsRef<str> for RepoId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RepoId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::borrow::Borrow<str> for RepoId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

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
    /// Architectures considered "installable" on this profile. The
    /// solver, file_owner, and binaries_built_from queries all
    /// filter results to this set so an x86_64 profile doesn't pin
    /// (or report ownership against) an i686 binary that happens to
    /// live in the same multilib repo. Empty = no filter (test
    /// universes / unknown profile arch).
    ///
    /// Build from `profile.arch.build_arch + "noarch"` at universe
    /// construction time.
    acceptable_archs: Vec<String>,
}

impl RepoUniverse {
    /// Build a universe from a list of opened DBs. Caller orders by
    /// priority. Reads the `meta` rows of each DB once to populate
    /// the snapshot id map.
    pub fn from_dbs(
        profile_name: impl Into<String>,
        dbs: Vec<RepoDb>,
    ) -> Result<Self, RepoError> {
        Self::from_dbs_with_arch(profile_name, dbs, Vec::new())
    }

    /// Construct a universe with an explicit arch filter — every
    /// query (`by_name`, `provides_by_name`, `file_owner`,
    /// `candidates_with_nevra`, `binaries_built_from`) limits its
    /// results to packages whose arch is in `acceptable_archs`.
    ///
    /// Pass `vec![profile.arch.build_arch, "noarch"]` from the CLI
    /// so an x86_64 profile doesn't pin i686 binaries from a
    /// multilib repo (and so the resolver doesn't pick wrong-arch
    /// candidates in the first place). Empty = no filter, which is
    /// what test universes use.
    pub fn from_dbs_with_arch(
        profile_name: impl Into<String>,
        dbs: Vec<RepoDb>,
        acceptable_archs: Vec<String>,
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
            acceptable_archs,
        })
    }

    /// `true` iff `arch` is acceptable on this profile. Empty filter
    /// = accept everything (test universes / arch-agnostic profiles).
    #[must_use]
    pub fn arch_accepted(&self, arch: &str) -> bool {
        self.acceptable_archs.is_empty() || self.acceptable_archs.iter().any(|a| a == arch)
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
    ///
    /// Filters by [`Self::arch_accepted`] — a multilib `i686` package
    /// owning `/usr/lib/foo.so.1` doesn't count as the owner of a
    /// file from the perspective of an `x86_64` profile (the i686
    /// binary lives in `/usr/lib`, the x86_64 binary in `/usr/lib64`;
    /// `dnf install` on x86_64 will only ever bring the x86_64
    /// counterpart, so the i686 owner is irrelevant).
    pub fn file_owner(&self, path: &str) -> Result<Option<ProviderRef>, RepoError> {
        for db in &self.dbs {
            // Multilib repos commonly list the same path under both
            // i686 and x86_64 packages (e.g. `/usr/bin/perl`). Use
            // the all-owners variant and pick the first
            // arch-acceptable one; falling back to `db.file_owner`'s
            // LIMIT 1 would let a wrong-arch owner shadow the
            // correct one and report the file as unowned.
            for (pkg_id, arch) in db.file_owners(path)? {
                if !self.arch_accepted(&arch) {
                    continue;
                }
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
                if !self.arch_accepted(&nevra.arch) {
                    continue;
                }
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
                if !self.arch_accepted(&nevra.arch) {
                    continue;
                }
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

    /// Every binary `(ProviderRef, NEVRA)` whose source RPM matches
    /// `source_name` across every cached repo in this universe. Drives
    /// `matrix upgrade-sim` and the RPM-REPO-030/031 lints — they ask
    /// "what's currently published for the source named X?" and pick
    /// the highest EVR per (subpackage name, arch).
    ///
    /// Returned order: insertion order across repos (priority unsorted
    /// — callers reduce as they see fit). Dedup is by `ProviderRef`.
    pub fn binaries_built_from(
        &self,
        source_name: &str,
    ) -> Result<Vec<(ProviderRef, NEVRA)>, RepoError> {
        use std::collections::HashSet;
        let mut seen: HashSet<ProviderRef> = HashSet::new();
        let mut out: Vec<(ProviderRef, NEVRA)> = Vec::new();
        for db in &self.dbs {
            let repo_id = db.repo_id()?;
            for (pkg_id, nevra) in db.pkg_briefs_by_source_name(source_name)? {
                if !self.arch_accepted(&nevra.arch) {
                    continue;
                }
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
        req: &crate::package::Dependency,
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

#[cfg(test)]
mod repo_id_tests {
    //! Contract tests for the [`RepoId`] newtype. The Borrow + Hash +
    //! Eq triple is load-bearing: `HashMap<RepoId, _>` lookups by
    //! `&str` only work if the three derived/manual impls agree.
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn from_str_and_string_compare_equal() {
        // The two public constructors must produce values that
        // compare equal (different allocation, same byte content).
        // Cross-source equality is what keeps `HashMap` lookups
        // working when the lookup key was built from a `&str` and
        // the inserted key from a `String`.
        let a = RepoId::from("baseos");
        let b = RepoId::from(String::from("baseos"));
        assert_eq!(a, b);
    }

    #[test]
    fn debug_matches_underlying_str_repr() {
        // Manual `Debug` impl: future tracing-field migrations
        // (`tracing::info!(?repo_id, …)`) must keep producing
        // `"baseos"`, not `RepoId("baseos")`, so structured-log
        // shippers don't have to learn the newtype.
        let id = RepoId::from("baseos");
        assert_eq!(format!("{id:?}"), "\"baseos\"");
    }

    #[test]
    fn btree_map_range_uses_str_ordering() {
        // `snapshot_ids: BTreeMap<RepoId, String>` is range-queried
        // by the lockfile writer; `Ord` on `RepoId` must therefore
        // agree with `Borrow<str>` so a range bounded by str
        // literals returns the same items it would for the inner
        // `Arc<str>`. Pin that explicitly.
        use std::collections::BTreeMap;
        let mut bt: BTreeMap<RepoId, ()> = BTreeMap::new();
        bt.insert(RepoId::from("appstream"), ());
        bt.insert(RepoId::from("baseos"), ());
        bt.insert(RepoId::from("updates"), ());
        let lo: &str = "b";
        let hi: &str = "z";
        let in_range: Vec<&str> = bt
            .range::<str, _>((std::ops::Bound::Included(lo), std::ops::Bound::Excluded(hi)))
            .map(|(k, _)| k.as_str())
            .collect();
        assert_eq!(in_range, vec!["baseos", "updates"]);
    }

    #[test]
    fn display_matches_underlying_str() {
        let id = RepoId::from("appstream");
        assert_eq!(format!("{id}"), "appstream");
        assert_eq!(id.as_str(), "appstream");
        assert_eq!(<RepoId as AsRef<str>>::as_ref(&id), "appstream");
    }

    #[test]
    fn hashmap_lookup_by_str_works() {
        // Borrow<str> + Hash + Eq agreement: required for the
        // `HashMap<RepoId, T>::get(name: &str)` pattern used by
        // `RepoUniverse::repo_by_id`.
        let mut map: HashMap<RepoId, i32> = HashMap::new();
        map.insert(RepoId::from("baseos"), 10);
        map.insert(RepoId::from("appstream"), 20);
        assert_eq!(map.get("baseos"), Some(&10));
        assert_eq!(map.get("nope"), None);
    }
}
