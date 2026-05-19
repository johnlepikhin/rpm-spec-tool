//! Per-repo indexes and the assembled per-profile universe.
//!
//! [`RepoIndex`] holds one snapshot of one repository. [`RepoUniverse`]
//! is the assembled view a resolver operates against: ordered list of
//! [`RepoIndex`]es plus auxiliary lookup tables (Provides → providers,
//! file → owner, reverse Requires).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

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

/// One parsed snapshot of one repository.
///
/// Like [`Package`](crate::package::Package), this is a leaf data
/// shape constructed by backends in `rpm-spec-repo-metadata` and is
/// intentionally not `#[non_exhaustive]`.
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

/// Cross-package reference into a [`RepoUniverse`]. 8 bytes
/// (`RepoId` is a clone of an `Arc<str>`, `pkg_idx` is a `u32`)
/// avoids cloning whole [`Package`] structs in the providers /
/// reverse-requires indexes.
///
/// `RepoId` is cloned as an `Arc` so cloning a `ProviderRef` is one
/// atomic refcount bump.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct ProviderRef {
    pub repo_id: RepoId,
    pub pkg_idx: u32,
}

impl ProviderRef {
    /// Look up the referenced package in the universe. Returns `None`
    /// if the universe doesn't contain the named repo (caller bug)
    /// or the index is out of bounds (data corruption).
    ///
    /// Delegates to [`RepoUniverse::resolve`] for the O(1) lookup —
    /// this method exists for ergonomic call-site syntax on
    /// `pref.resolve(&universe)`.
    #[must_use]
    pub fn resolve<'u>(&self, universe: &'u RepoUniverse) -> Option<&'u Package> {
        universe.resolve(self)
    }
}

/// Assembled per-profile package universe. Lookup tables are built
/// once during [`RepoUniverse::build`] and shared via `Arc`. The
/// universe itself is constructed in `rpm-spec-analyzer`'s
/// `RepoSession` from a list of resolved [`RepoIndex`]es.
#[derive(Debug)]
#[non_exhaustive]
pub struct RepoUniverse {
    pub profile_name: String,
    /// Repos in priority order (ascending — lowest wins ties).
    pub repos: Vec<Arc<RepoIndex>>,

    /// Capability name → all providers across repos, sorted by
    /// (priority asc, EVR desc, name lex). Resolver picks the head.
    pub provides_by_name: HashMap<Arc<str>, Vec<ProviderRef>>,

    /// Package name → packages with that name, for direct `Requires:
    /// foo` lookups.
    pub by_name: HashMap<Arc<str>, Vec<ProviderRef>>,

    /// File path → owning package. Populated from filelists.xml
    /// (rpm-md) or contents_index (apt-rpm). Empty when filelists
    /// haven't been loaded yet — `repo sync` populates eagerly per
    /// M1 decision.
    pub file_owner: HashMap<Arc<str>, ProviderRef>,

    /// Reverse Requires index: capability name → packages that
    /// require it. Used by reverse-dep impact lints in P1.
    pub reverse_requires: HashMap<Arc<str>, Vec<ProviderRef>>,

    /// Snapshot ids per repo — recorded into the lockfile when the
    /// universe is locked.
    pub snapshot_ids: BTreeMap<RepoId, String>,

    /// Index from repo id to slice position in `repos`. Built once by
    /// `build()` so `ProviderRef::resolve` is O(1).
    pub repo_by_id: HashMap<RepoId, usize>,
}

impl RepoUniverse {
    /// Build a universe from a list of indexes, computing all lookup
    /// tables. The caller is responsible for ordering `indexes` by
    /// priority (the assembler in `rpm-spec-analyzer` sorts before
    /// calling).
    #[must_use]
    pub fn build(profile_name: impl Into<String>, indexes: Vec<Arc<RepoIndex>>) -> Self {
        let mut provides_by_name: HashMap<Arc<str>, Vec<ProviderRef>> = HashMap::new();
        let mut by_name: HashMap<Arc<str>, Vec<ProviderRef>> = HashMap::new();
        let mut file_owner: HashMap<Arc<str>, ProviderRef> = HashMap::new();
        let mut reverse_requires: HashMap<Arc<str>, Vec<ProviderRef>> = HashMap::new();
        let mut snapshot_ids: BTreeMap<RepoId, String> = BTreeMap::new();

        for idx in &indexes {
            snapshot_ids.insert(idx.repo_id.clone(), idx.revision.clone());

            for (i, pkg) in idx.packages.iter().enumerate() {
                let pref = ProviderRef {
                    repo_id: idx.repo_id.clone(),
                    pkg_idx: u32::try_from(i).expect("repo index < 4 billion packages"),
                };

                by_name
                    .entry(pkg.nevra.name.clone())
                    .or_default()
                    .push(pref.clone());

                for prov in &pkg.provides {
                    provides_by_name
                        .entry(prov.name.clone())
                        .or_default()
                        .push(pref.clone());
                }

                for req in &pkg.requires {
                    reverse_requires
                        .entry(req.name.clone())
                        .or_default()
                        .push(pref.clone());
                }

                for f in &pkg.files {
                    // First writer wins; conflict-detection lints
                    // (RPM-REPO-020 / RPM-REPO-071) scan the raw
                    // packages later. The owner map answers
                    // "which package owns /usr/bin/X" — for the rare
                    // multi-owner case higher-priority repo is
                    // canonical, matching dnf.
                    file_owner.entry(f.clone()).or_insert_with(|| pref.clone());
                }
            }
        }

        let repo_by_id: HashMap<RepoId, usize> = indexes
            .iter()
            .enumerate()
            .map(|(i, idx)| (idx.repo_id.clone(), i))
            .collect();

        Self {
            profile_name: profile_name.into(),
            repos: indexes,
            provides_by_name,
            by_name,
            file_owner,
            reverse_requires,
            snapshot_ids,
            repo_by_id,
        }
    }

    /// Total package count across all indexed repos. Cheap.
    #[must_use]
    pub fn package_count(&self) -> usize {
        self.repos.iter().map(|i| i.packages.len()).sum()
    }

    /// O(1) lookup of a package by its [`ProviderRef`].
    #[must_use]
    pub fn resolve(&self, pref: &ProviderRef) -> Option<&Package> {
        let i = self.repo_by_id.get(&pref.repo_id).copied()?;
        self.repos.get(i)?.packages.get(pref.pkg_idx as usize)
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
