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

impl CapFlags {
    /// Map a three-way comparison (`provider_evr.cmp(required_evr)`)
    /// onto a `CapFlags` predicate. The single source of truth for
    /// EVR-vs-flag matching — both [`crate::db::RepoDb`] (SQLite
    /// satisfies path) and `repo-resolver` (lookup / solver) call
    /// through here so the contract can't drift.
    #[must_use]
    pub fn matches(self, cmp: std::cmp::Ordering) -> bool {
        use std::cmp::Ordering::{Equal, Greater, Less};
        match self {
            CapFlags::None => true,
            CapFlags::EQ => cmp == Equal,
            CapFlags::LT => cmp == Less,
            CapFlags::LE => cmp != Greater,
            CapFlags::GT => cmp == Greater,
            CapFlags::GE => cmp != Less,
        }
    }
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

impl Capability {
    /// `true` when this capability is a file-path requirement
    /// (`Requires: /usr/bin/foo`). rpm-md publishes file ownership
    /// via the per-package filelist, so callers must route these
    /// through `RepoUniverse::file_owner` instead of the normal
    /// Provides lookup. Centralised so a future "treat `file://`
    /// as a path too" change is one edit, not six.
    #[must_use]
    pub fn is_file_path(&self) -> bool {
        self.name.starts_with('/')
    }

    /// Render as the user typed it: `name op E:V-R` with optional
    /// parts elided. Matches the form most rpm tools (`rpm -q`,
    /// `dnf repoquery`) print, minus the arch suffix which callers
    /// may need to bolt on themselves (rpm-md stores arch on the
    /// package, not as part of the capability name).
    #[must_use]
    pub fn display(&self) -> String {
        use std::fmt::Write as _;
        let mut s = self.name.to_string();
        if let Some(evr) = &self.evr {
            // `CapFlags::None` + `Some(evr)` is a parser bug: the
            // unconstrained sense should never carry a version. Assert
            // in debug to catch backend regressions; in release fall
            // back to the unconstrained form (no operator) so we never
            // emit a silent `?` token to the user.
            debug_assert!(
                !matches!(self.flags, CapFlags::None),
                "Capability::display: CapFlags::None must not carry evr ({})",
                self.name
            );
            let op = match self.flags {
                CapFlags::LT => Some("<"),
                CapFlags::LE => Some("<="),
                CapFlags::EQ => Some("="),
                CapFlags::GE => Some(">="),
                CapFlags::GT => Some(">"),
                CapFlags::None => None,
            };
            let Some(op) = op else { return s };
            s.push(' ');
            s.push_str(op);
            s.push(' ');
            if evr.epoch > 0 {
                // `write!` into the existing String avoids the
                // intermediate allocation `push_str(&format!(...))`
                // would create. Infallible on a String sink.
                let _ = write!(&mut s, "{}:", evr.epoch);
            }
            s.push_str(&evr.version);
            if !evr.release.is_empty() {
                s.push('-');
                s.push_str(&evr.release);
            }
        }
        s
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(name: &str, flags: CapFlags, evr: Option<EVR>) -> Capability {
        Capability {
            name: Arc::from(name),
            flags,
            evr,
        }
    }

    #[test]
    fn is_file_path_recognises_absolute_paths() {
        assert!(cap("/usr/bin/bash", CapFlags::None, None).is_file_path());
        assert!(cap("/etc/passwd", CapFlags::None, None).is_file_path());
        assert!(!cap("bash", CapFlags::None, None).is_file_path());
        assert!(!cap("pkgconfig(foo)", CapFlags::None, None).is_file_path());
        // A leading-slash provide is treated as a path even with version
        // — the lookup path is selected by name shape, not by flags.
        assert!(cap("/lib64/libc.so.6", CapFlags::EQ, None).is_file_path());
    }

    #[test]
    fn display_unconstrained_capability() {
        assert_eq!(cap("foo", CapFlags::None, None).display(), "foo");
    }

    #[test]
    fn display_versioned_capability() {
        let evr = EVR::new(None, "1.2", "3.el9");
        assert_eq!(cap("foo", CapFlags::GE, Some(evr)).display(), "foo >= 1.2-3.el9");
    }

    #[test]
    fn display_with_epoch() {
        let evr = EVR::new(Some(2), "1.0", "1");
        assert_eq!(cap("foo", CapFlags::EQ, Some(evr)).display(), "foo = 2:1.0-1");
    }

    #[test]
    fn display_without_release() {
        let evr = EVR::new(None, "1.2", "");
        assert_eq!(cap("foo", CapFlags::LT, Some(evr)).display(), "foo < 1.2");
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn display_no_flag_with_evr_falls_back_in_release() {
        // Release builds must never emit the `?` sentinel; the
        // unreachable arm degrades to the unconstrained form.
        let evr = EVR::new(None, "1.2", "");
        assert_eq!(cap("foo", CapFlags::None, Some(evr)).display(), "foo");
    }
}
