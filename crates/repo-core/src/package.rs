//! Per-package data extracted from primary.xml / pkglist.classic.
//!
//! `Package` is a leaf type: it does not borrow into a parent index.
//! The big-string fields (`name`, `arch`, file paths) are `Arc<str>`
//! so two packages with the same name/arch share one allocation
//! across the whole [`crate::index::RepoUniverse`]. With
//! ~60k packages per Fedora-scale repo and aggressive Arc reuse the
//! in-memory footprint stays under ~100 MB per profile.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::evr::EVR;
use crate::index::RepoId;

/// Name-Epoch-Version-Release-Arch — the canonical RPM identity tuple.
///
/// `epoch` is stored as `u32` (rpm treats an absent epoch and `0` as
/// equivalent; mirroring [`EVR`]'s convention keeps `Hash`/`PartialEq`
/// agreeing with rpm semantics so two NEVRAs that only differ in
/// "explicit 0" vs "absent" hash the same).
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct NEVRA {
    pub name: Arc<str>,
    #[serde(default)]
    pub epoch: u32,
    pub version: Arc<str>,
    pub release: Arc<str>,
    pub arch: Arc<str>,
}

impl NEVRA {
    #[must_use]
    pub fn evr(&self) -> EVR {
        EVR::new(Some(self.epoch), self.version.as_ref(), self.release.as_ref())
    }
}

impl std::fmt::Display for NEVRA {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Print `name-VER-REL.arch`; prepend `epoch:` only when the
        // epoch is non-zero. Matches rpm's canonical short form and
        // avoids the noisy `name-0:VER-REL.arch` shape that the old
        // explicit-0 branch produced.
        if self.epoch > 0 {
            write!(
                f,
                "{}-{}:{}-{}.{}",
                self.name, self.epoch, self.version, self.release, self.arch
            )
        } else {
            write!(
                f,
                "{}-{}-{}.{}",
                self.name, self.version, self.release, self.arch
            )
        }
    }
}

/// Sense flags on a [`Capability`]. The wire format from primary.xml
/// uses string tokens; this enum keeps them packed.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum CapFlags {
    /// No version constraint — `Requires: foo` form.
    None,
    EQ,
    LT,
    LE,
    GT,
    GE,
}

/// One RPM capability: either a `Provides:` entry or a constraint on
/// a `Requires:` / `Conflicts:` / `Obsoletes:` entry. The same shape
/// covers both directions; the discriminator is which Vec the value
/// lives in on [`Package`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capability {
    pub name: Arc<str>,
    pub flags: CapFlags,
    /// `None` when [`CapFlags::None`]. Otherwise the constraint EVR.
    pub evr: Option<EVR>,
}

/// `Requires`/`Conflicts`/`Obsoletes` entries share Capability's shape.
/// Named for caller-site readability.
pub type Dependency = Capability;

/// Cryptographic checksum of the .rpm payload. Stored mainly so cache
/// validation can compare against repomd's recorded digest when the
/// .rpm is later downloaded (P0 does not download .rpm bodies — only
/// metadata).
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum PkgChecksum {
    Sha256(String),
    Sha1(String),
    /// Unknown algorithm carried through verbatim so we don't lose
    /// information when a repo uses something we don't validate yet.
    Other { algo: String, hex: String },
}

/// One package as parsed from primary.xml (and, when filelists are
/// loaded, with `files` populated). Lives inside a
/// [`crate::index::RepoIndex`]; cross-package references use
/// [`crate::index::ProviderRef`] (8 bytes) instead of cloning.
///
/// The field shape mirrors the rpm-md / apt-rpm metadata formats and
/// is intentionally NOT `#[non_exhaustive]` — backends in
/// `rpm-spec-repo-metadata` construct it via struct literal during
/// parsing. New fields are added cautiously and require a coordinated
/// release across the workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Package {
    pub nevra: NEVRA,
    pub repo_id: RepoId,

    pub provides: Vec<Capability>,
    pub requires: Vec<Dependency>,
    pub conflicts: Vec<Dependency>,
    pub obsoletes: Vec<Dependency>,
    /// Weak deps — parsed in P0 for completeness but not consumed by
    /// the resolver until a later milestone adds weak-dep lints.
    pub recommends: Vec<Dependency>,
    pub suggests: Vec<Dependency>,
    pub supplements: Vec<Dependency>,
    pub enhances: Vec<Dependency>,

    /// Source RPM name (`bash-5.1.8-9.el9.src.rpm`). Used by
    /// `matrix upgrade-sim` to map a spec to its current binary
    /// publications in the repo.
    pub source_rpm: Option<Arc<str>>,

    pub summary: Arc<str>,
    pub size_installed: u64,
    pub checksum: PkgChecksum,
    /// Location relative to repo baseurl (e.g. `Packages/b/bash-5.1.8-...rpm`).
    pub location: Arc<str>,

    /// File paths owned by this package. Populated by filelists.xml
    /// (rpm-md) or contents_index (apt-rpm). Empty until the eager
    /// load pass during `repo sync`.
    pub files: Vec<Arc<str>>,
}

impl Package {
    /// Convenience: does this package's name + arch match a target?
    /// Used by the resolver when a Requires names a package directly
    /// (rather than a virtual capability).
    #[must_use]
    pub fn matches_name(&self, name: &str) -> bool {
        self.nevra.name.as_ref() == name
    }
}
