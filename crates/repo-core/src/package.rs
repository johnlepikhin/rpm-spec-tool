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
        EVR::new(
            Some(self.epoch),
            self.version.as_ref(),
            self.release.as_ref(),
        )
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

/// Version constraint on a [`Capability`] — parse-don't-validate
/// replacement for the old `flags: CapFlags + evr: Option<EVR>` pair.
///
/// The old shape allowed `CapFlags::None + Some(evr)` and
/// `CapFlags::EQ + None`; both were parser bugs but cost a runtime
/// `debug_assert!` to catch. With the enum, those states cease to
/// exist at the type level.
///
/// Variants carry the EVR by value. There's no `Box`-wrapping
/// because `EVR` is ~64 bytes (three `String` + `u32`), comparable
/// to `Option<EVR>` after niche optimisation — the enum is no larger
/// than the struct it replaces.
///
/// Serde format mirrors the JSON shape downstream tools expect: a
/// `tag = "op"` discriminant with the EVR fields inlined. Backward
/// compat with the iter4 wire format is **not** preserved (no
/// downstream consumer exists yet); the SQLite layer in
/// `crate::db::convert` translates explicitly between this enum and
/// the historical `flags TEXT + epoch/version/release` columns so
/// existing on-disk snapshots load unchanged.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CapVersion {
    /// `Requires: foo` — name match only, no EVR constraint.
    Unversioned,
    /// `Requires: foo = E:V-R`.
    Eq(EVR),
    /// `Requires: foo < E:V-R`.
    Lt(EVR),
    /// `Requires: foo <= E:V-R`.
    Le(EVR),
    /// `Requires: foo > E:V-R`.
    Gt(EVR),
    /// `Requires: foo >= E:V-R`.
    Ge(EVR),
}

impl CapVersion {
    /// Borrow the constraint's EVR, if any. `Unversioned` returns
    /// `None`; every other variant returns `Some`.
    #[must_use]
    pub fn evr(&self) -> Option<&EVR> {
        match self {
            Self::Unversioned => None,
            Self::Eq(e) | Self::Lt(e) | Self::Le(e) | Self::Gt(e) | Self::Ge(e) => Some(e),
        }
    }

    /// Operator string the user types (`=`, `<=`, `>=`, etc.).
    /// `Unversioned` returns `None`. Display helpers use this; the
    /// resolver doesn't need it.
    #[must_use]
    pub fn op_str(&self) -> Option<&'static str> {
        match self {
            Self::Unversioned => None,
            Self::Eq(_) => Some("="),
            Self::Lt(_) => Some("<"),
            Self::Le(_) => Some("<="),
            Self::Gt(_) => Some(">"),
            Self::Ge(_) => Some(">="),
        }
    }

    /// `true` iff a provider whose EVR compares to this constraint's
    /// EVR with `cmp` satisfies the constraint.
    ///
    /// Single source of truth for "does provider EVR X satisfy
    /// require X op Y" — both `crate::db::RepoDb` (SQLite satisfies
    /// path) and `repo-resolver` (lookup / solver) route through
    /// here. `Unversioned` matches any provider unconditionally.
    #[must_use]
    pub fn matches(&self, cmp: std::cmp::Ordering) -> bool {
        use std::cmp::Ordering::{Equal, Greater, Less};
        match self {
            Self::Unversioned => true,
            Self::Eq(_) => cmp == Equal,
            Self::Lt(_) => cmp == Less,
            Self::Le(_) => cmp != Greater,
            Self::Gt(_) => cmp == Greater,
            Self::Ge(_) => cmp != Less,
        }
    }

    /// `true` when this is `Unversioned` — convenience for the
    /// "is this an EVR-less name match" predicate that appears in
    /// every solver/lookup hot path.
    #[must_use]
    pub fn is_unversioned(&self) -> bool {
        matches!(self, Self::Unversioned)
    }
}

/// One RPM `Provides:` entry — a `(name, version-constraint)` pair
/// a package supplies. The require-side direction (`Requires:` /
/// `Conflicts:` / `Obsoletes:` / weak-deps / spec `BuildRequires:`)
/// shares the same `(name, version)` syntactic shape but lives
/// behind the [`Dependency`] newtype so a single resolver function
/// can't accept both directions interchangeably (the prior
/// `pub type Dependency = Capability` alias allowed Provides-shaped
/// values to flow into Require-shaped slots silently — see
/// `Dependency`'s doc for the rationale).
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct Capability {
    pub name: Arc<str>,
    /// Version constraint. `Unversioned` for unconstrained
    /// `Requires: foo` form; otherwise a variant carrying the EVR.
    pub version: CapVersion,
}

/// A required-side capability — `Requires:` / `Conflicts:` /
/// `Obsoletes:` / weak-dep entry on a package, or a literal
/// `BuildRequires:` line off a spec. Same syntactic shape as
/// [`Capability`] (which models the `Provides:` direction), wrapped
/// in a newtype so the type system catches "passed Provides where
/// Requires expected" bugs at every function boundary.
///
/// Read access uses inherent forwarders ([`Self::name`],
/// [`Self::version`], [`Self::is_file_path`], [`Self::display`]) —
/// see the `# Conversions` section below for why
/// `Deref<Target = Capability>` is deliberately absent. Cloning is
/// O(1) (one `Arc<str>` refcount bump for the name, plus the
/// inline `CapVersion` enum copy).
///
/// **When to use which:**
/// - [`Capability`] for `Package.provides` and any "this is what a
///   package supplies" position.
/// - [`Dependency`] for `Package.requires` / `conflicts` /
///   `obsoletes` / `recommends` / `suggests` / `supplements` /
///   `enhances` and for the resolver's `requirements` /
///   `base_packages` / `implicit_brs` slots — the "this is what a
///   package or build needs" position.
///
/// # Conversions
///
/// - `Capability → Dependency`: explicit via `Dependency::new(cap)`
///   or `cap.into()` (both fire the same `From<Capability>` impl,
///   which is the only direction with a `From` to keep the strong
///   require-side claim at the type-system layer).
/// - `&Dependency → &Capability`: explicit via
///   [`Self::as_capability`] when an `&Capability`-shaped API
///   (e.g. the DB writer) genuinely needs the unwrapped value.
/// - Read access (`.name()`, `.version()`, `.is_file_path()`,
///   `.display()`): inherent forwarders on `Dependency`, not via
///   `Deref`. `Deref<Target = Capability>` would silently coerce
///   `&Dependency` into `&Capability` at every callsite, defeating
///   the whole point of the newtype — so it's deliberately absent.
#[derive(Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Dependency(Capability);

impl Dependency {
    /// Wrap an existing [`Capability`] in the requires-side newtype.
    /// Equivalent to `cap.into()` (via `From<Capability>`); pick the
    /// form that reads better at the call site — `Dependency::new`
    /// when the type is non-obvious, `.into()` in iterator chains.
    #[must_use]
    pub fn new(cap: Capability) -> Self {
        Self(cap)
    }

    /// Unversioned requirement (`Requires: foo`). Mirror of
    /// [`Capability::unversioned`] so test code and builders don't
    /// have to wrap.
    #[must_use]
    pub fn unversioned(name: impl Into<Arc<str>>) -> Self {
        Self(Capability::unversioned(name))
    }

    /// `name >= E:V-R` requirement. Mirrors [`Capability::ge`].
    #[must_use]
    pub fn ge(name: impl Into<Arc<str>>, evr: EVR) -> Self {
        Self(Capability::ge(name, evr))
    }

    /// `name = E:V-R` requirement. Mirrors [`Capability::eq`].
    /// `lt` / `le` / `gt` are unimplemented — add them when a
    /// caller appears (YAGNI).
    #[must_use]
    pub fn eq(name: impl Into<Arc<str>>, evr: EVR) -> Self {
        Self(Capability::eq(name, evr))
    }

    /// Borrow the underlying `Capability` — for sites that need to
    /// hand the unwrapped shape to a function generic over
    /// `Capability` (e.g. the DB writer feeding `insert_cap_batch`).
    /// **Read access** (`.name()`, `.version()`, `.display()`,
    /// `.is_file_path()`) should go through the inherent methods on
    /// `Dependency` instead so the require-side boundary stays
    /// visible at every call site.
    #[must_use]
    pub fn as_capability(&self) -> &Capability {
        &self.0
    }

    /// Borrow the dependency's name. Forwards to `Capability::name`
    /// without an `as_capability()` indirection at the call site.
    #[must_use]
    pub fn name(&self) -> &Arc<str> {
        &self.0.name
    }

    /// Borrow the dependency's version constraint. Forwards to
    /// [`Capability::version`].
    #[must_use]
    pub fn version(&self) -> &CapVersion {
        &self.0.version
    }

    /// File-path requirement check — forwards to
    /// [`Capability::is_file_path`].
    #[must_use]
    pub fn is_file_path(&self) -> bool {
        self.0.is_file_path()
    }

    /// User-facing render of the dependency. Forwards to
    /// [`Capability::display`].
    #[must_use]
    pub fn display(&self) -> String {
        self.0.display()
    }
}

// Manual `Debug` so log entries keep producing the inner
// `Capability { name, version }` shape rather than wrapping it in
// `Dependency(Capability { … })` — observability consumers that
// grep the existing format don't have to learn the newtype.
impl std::fmt::Debug for Dependency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0, f)
    }
}

impl From<Capability> for Dependency {
    fn from(cap: Capability) -> Self {
        Self(cap)
    }
}

impl Capability {
    /// Constructor for the unversioned form. Shorthand for tests and
    /// the common `Requires: foo` projection.
    #[must_use]
    pub fn unversioned(name: impl Into<Arc<str>>) -> Self {
        Self {
            name: name.into(),
            version: CapVersion::Unversioned,
        }
    }

    /// Constructor for `name = E:V-R`. The five `eq`/`lt`/`le`/`gt`/`ge`
    /// helpers mirror the [`CapVersion`] variant set so callers don't
    /// repeat the `Capability { name, version: CapVersion::Eq(evr) }`
    /// struct-literal boilerplate at every test or builder site.
    #[must_use]
    pub fn eq(name: impl Into<Arc<str>>, evr: EVR) -> Self {
        Self {
            name: name.into(),
            version: CapVersion::Eq(evr),
        }
    }
    /// Constructor for `name < E:V-R`. See [`Self::eq`].
    #[must_use]
    pub fn lt(name: impl Into<Arc<str>>, evr: EVR) -> Self {
        Self {
            name: name.into(),
            version: CapVersion::Lt(evr),
        }
    }
    /// Constructor for `name <= E:V-R`. See [`Self::eq`].
    #[must_use]
    pub fn le(name: impl Into<Arc<str>>, evr: EVR) -> Self {
        Self {
            name: name.into(),
            version: CapVersion::Le(evr),
        }
    }
    /// Constructor for `name > E:V-R`. See [`Self::eq`].
    #[must_use]
    pub fn gt(name: impl Into<Arc<str>>, evr: EVR) -> Self {
        Self {
            name: name.into(),
            version: CapVersion::Gt(evr),
        }
    }
    /// Constructor for `name >= E:V-R`. See [`Self::eq`].
    #[must_use]
    pub fn ge(name: impl Into<Arc<str>>, evr: EVR) -> Self {
        Self {
            name: name.into(),
            version: CapVersion::Ge(evr),
        }
    }

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
        let Some(op) = self.version.op_str() else {
            return s;
        };
        let Some(evr) = self.version.evr() else {
            // Unreachable given `op_str()` only returns `Some` for
            // variants that carry an EVR — but the enum is
            // `#[non_exhaustive]` so the compiler can't prove that.
            // Falling back to the bare name is the safe degradation.
            return s;
        };
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
        s
    }
}

/// Cryptographic checksum of the .rpm payload. Stored mainly so cache
/// validation can compare against repomd's recorded digest when the
/// .rpm is later downloaded (P0 does not download .rpm bodies — only
/// metadata).
///
/// Construct via [`PkgChecksum::try_new`] for any input that crosses
/// a trust boundary (rpm-md `<rpm:checksum type="...">`, apt-rpm
/// pkglist header). The constructor validates the hex string length
/// matches the algorithm (sha256 → 64, sha1 → 40, md5 → 32), rejects
/// non-hex characters, and stores the resulting hex in **canonical
/// lowercase form**; unknown algorithm names are preserved verbatim
/// in [`PkgChecksum::Other`] for forward-compat but their hex is
/// still hex-validated.
///
/// Direct struct-literal construction is permitted but should be
/// confined to **tests** and **explicit-sentinel** sites (e.g. the
/// "missing checksum block" fallback in `rpmmd::primary`, which
/// inserts an empty `Other { algo: "unknown", hex: "" }`
/// deliberately to keep the package queryable). Any untrusted input
/// must route through `try_new`.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum PkgChecksum {
    Sha256(String),
    Sha1(String),
    /// Unknown algorithm carried through verbatim so we don't lose
    /// information when a repo uses something we don't validate yet.
    Other {
        algo: String,
        hex: String,
    },
}

/// Validation failure from [`PkgChecksum::try_new`]. The variant
/// carries the offending pair so a caller logging the error can
/// attribute the failure to a specific package.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ChecksumParseError {
    /// Hex string length doesn't match the algorithm's digest size.
    #[error("checksum length mismatch for `{algo}`: expected {expected} hex chars, got {got}")]
    BadLength {
        algo: String,
        expected: usize,
        got: usize,
    },
    /// Hex string contains a non-`[0-9a-fA-F]` byte.
    #[error("checksum for `{algo}` contains non-hex character: {hex:?}")]
    NotHex { algo: String, hex: String },
    /// Empty algorithm name. We tolerate unknown algorithm names but
    /// not literally empty ones.
    #[error("checksum algorithm is empty")]
    EmptyAlgorithm,
}

/// Known digest algorithms with their hex-string length. Centralised
/// so adding SHA-512 (or any other new variant) is a single-line
/// extension that automatically catches the dispatch arm via the
/// loop below.
const KNOWN_DIGEST_LENGTHS: &[(&str, usize)] = &[("sha256", 64), ("sha1", 40), ("md5", 32)];

impl PkgChecksum {
    /// Validate `(algo, hex)` and construct a [`PkgChecksum`]. The
    /// algorithm name is lowercased before dispatch so callers don't
    /// have to canonicalise (rpm-md primary.xml occasionally ships
    /// `SHA256` uppercased); the resulting `Sha256(hex)` variant
    /// **stores hex in canonical lowercase form** so byte-equal
    /// comparison (`==`) and `Hash` agree across input cases.
    ///
    /// The method follows Rust's `try_new` convention for many-arg
    /// validating constructors. `parse` is kept as a deprecated alias
    /// for callers that haven't migrated yet.
    ///
    /// # Errors
    ///
    /// * [`ChecksumParseError::EmptyAlgorithm`] — algorithm is empty.
    /// * [`ChecksumParseError::NotHex`] — hex string contains a byte
    ///   outside `[0-9a-fA-F]`. The stored `hex` field is truncated to
    ///   at most 80 chars to bound allocation on hostile input.
    /// * [`ChecksumParseError::BadLength`] — hex length doesn't match
    ///   the well-known digest size for `algo` (sha256: 64, sha1: 40,
    ///   md5: 32). Unknown algorithms skip the length check and land
    ///   in [`PkgChecksum::Other`] for forward-compat.
    pub fn try_new(algo: &str, hex: &str) -> Result<Self, ChecksumParseError> {
        let algo_lc = algo.to_ascii_lowercase();
        if algo_lc.is_empty() {
            return Err(ChecksumParseError::EmptyAlgorithm);
        }
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            // Cap the captured hex at 80 chars: real digests are <= 128
            // (sha512), legitimate non-hex inputs are short typos. A
            // hostile mirror sending megabytes of garbage as a
            // "checksum" would otherwise allocate proportionally in
            // the error path. ASCII-only slice safe because non-hex
            // implies any byte, but a head-byte slice on a `&str`
            // would panic on a multibyte boundary — use `chars()`.
            let truncated: String = hex.chars().take(80).collect();
            return Err(ChecksumParseError::NotHex {
                algo: algo_lc,
                hex: truncated,
            });
        }
        if let Some(&(_, expected)) = KNOWN_DIGEST_LENGTHS
            .iter()
            .find(|(name, _)| *name == algo_lc.as_str())
            && hex.len() != expected
        {
            return Err(ChecksumParseError::BadLength {
                algo: algo_lc,
                expected,
                got: hex.len(),
            });
        }
        // Single hex lowercase pass (was three identical
        // `.to_ascii_lowercase()` calls; now one shared buffer).
        let hex_lc = hex.to_ascii_lowercase();
        Ok(match algo_lc.as_str() {
            "sha256" => Self::Sha256(hex_lc),
            "sha1" => Self::Sha1(hex_lc),
            _ => Self::Other {
                algo: algo_lc,
                hex: hex_lc,
            },
        })
    }

    /// Deprecated alias for [`Self::try_new`]. The `parse` name
    /// collided with Rust's `FromStr::parse` convention; new code
    /// should use `try_new`.
    #[deprecated(note = "use `PkgChecksum::try_new` for clarity (parse is the FromStr name)")]
    pub fn parse(algo: &str, hex: &str) -> Result<Self, ChecksumParseError> {
        Self::try_new(algo, hex)
    }
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

    fn cap(name: &str, version: CapVersion) -> Capability {
        Capability {
            name: Arc::from(name),
            version,
        }
    }

    #[test]
    fn is_file_path_recognises_absolute_paths() {
        assert!(cap("/usr/bin/bash", CapVersion::Unversioned).is_file_path());
        assert!(cap("/etc/passwd", CapVersion::Unversioned).is_file_path());
        assert!(!cap("bash", CapVersion::Unversioned).is_file_path());
        assert!(!cap("pkgconfig(foo)", CapVersion::Unversioned).is_file_path());
        // A leading-slash provide is treated as a path even with version
        // — the lookup path is selected by name shape, not by version.
        let evr = EVR::new(None, "1.0", "1");
        assert!(cap("/lib64/libc.so.6", CapVersion::Eq(evr)).is_file_path());
    }

    #[test]
    fn display_unconstrained_capability() {
        assert_eq!(cap("foo", CapVersion::Unversioned).display(), "foo");
    }

    #[test]
    fn display_versioned_capability() {
        let evr = EVR::new(None, "1.2", "3.el9");
        assert_eq!(
            cap("foo", CapVersion::Ge(evr)).display(),
            "foo >= 1.2-3.el9"
        );
    }

    #[test]
    fn display_with_epoch() {
        let evr = EVR::new(Some(2), "1.0", "1");
        assert_eq!(cap("foo", CapVersion::Eq(evr)).display(), "foo = 2:1.0-1");
    }

    #[test]
    fn display_without_release() {
        let evr = EVR::new(None, "1.2", "");
        assert_eq!(cap("foo", CapVersion::Lt(evr)).display(), "foo < 1.2");
    }

    #[test]
    fn cap_version_matches_orderings() {
        use std::cmp::Ordering::{Equal, Greater, Less};
        let evr = EVR::new(None, "1.0", "1");
        assert!(CapVersion::Unversioned.matches(Less));
        assert!(CapVersion::Unversioned.matches(Equal));
        assert!(CapVersion::Unversioned.matches(Greater));
        assert!(CapVersion::Eq(evr.clone()).matches(Equal));
        assert!(!CapVersion::Eq(evr.clone()).matches(Greater));
        assert!(CapVersion::Ge(evr.clone()).matches(Equal));
        assert!(CapVersion::Ge(evr.clone()).matches(Greater));
        assert!(!CapVersion::Ge(evr.clone()).matches(Less));
        assert!(CapVersion::Le(evr.clone()).matches(Less));
        assert!(CapVersion::Le(evr.clone()).matches(Equal));
        assert!(!CapVersion::Le(evr).matches(Greater));
    }

    // `cap_version_db_tag_round_trip` moved to `crate::db::convert`
    // tests as part of relocating the SQL-tag mapping out of the
    // domain type.

    #[test]
    fn pkg_checksum_parse_validates_sha256_length() {
        // 64 hex chars = canonical SHA-256.
        let valid = "a".repeat(64);
        let c = PkgChecksum::try_new("sha256", &valid).unwrap();
        assert!(matches!(c, PkgChecksum::Sha256(ref h) if h.len() == 64));

        // 63 chars — short by one. Must reject; silently truncating
        // would make distinct packages collide on lookup.
        let short = "a".repeat(63);
        let err = PkgChecksum::try_new("sha256", &short).unwrap_err();
        match err {
            ChecksumParseError::BadLength { expected, got, .. } => {
                assert_eq!(expected, 64);
                assert_eq!(got, 63);
            }
            other => panic!("expected BadLength, got {other:?}"),
        }
    }

    #[test]
    fn pkg_checksum_parse_rejects_non_hex() {
        // Non-hex char inside an otherwise length-correct sha256 hex.
        let mut bad = "a".repeat(63);
        bad.push('z');
        let err = PkgChecksum::try_new("sha256", &bad).unwrap_err();
        assert!(matches!(err, ChecksumParseError::NotHex { .. }));
    }

    #[test]
    fn pkg_checksum_parse_normalises_algorithm_case() {
        // rpm-md primary.xml occasionally ships `SHA256` uppercased.
        // The validating constructor canonicalises to lowercase and
        // routes through the strong `Sha256` variant, not `Other`.
        let valid = "f".repeat(64);
        let c = PkgChecksum::try_new("SHA256", &valid).unwrap();
        assert!(matches!(c, PkgChecksum::Sha256(_)));
    }

    #[test]
    fn pkg_checksum_parse_preserves_unknown_algorithm_without_length_check() {
        // Future / vendor-specific algorithms (e.g. ALT's GOST
        // variants) shouldn't be rejected just because we don't know
        // their digest length. Lands in `Other` verbatim.
        let c = PkgChecksum::try_new("gost-stribog-512", "deadbeef").unwrap();
        assert!(matches!(
            c,
            PkgChecksum::Other { ref algo, .. } if algo == "gost-stribog-512"
        ));
    }

    #[test]
    fn pkg_checksum_parse_rejects_empty_algorithm() {
        let err = PkgChecksum::try_new("", "abc123").unwrap_err();
        assert!(matches!(err, ChecksumParseError::EmptyAlgorithm));
    }

    #[test]
    fn pkg_checksum_parse_sha1_length() {
        let valid = "1".repeat(40);
        let c = PkgChecksum::try_new("sha1", &valid).unwrap();
        assert!(matches!(c, PkgChecksum::Sha1(_)));
        let short = "1".repeat(39);
        assert!(matches!(
            PkgChecksum::try_new("sha1", &short).unwrap_err(),
            ChecksumParseError::BadLength {
                expected: 40,
                got: 39,
                ..
            }
        ));
    }

    #[test]
    fn alt_set_version_ge_constraint_satisfied_by_any_set_provide() {
        // End-to-end check that the EVR `set:`-short-circuit composes
        // with `CapVersion::Ge` to satisfy ALT soname constraints.
        // The real-world require shape is
        //   `libpcre2-8.so.0()(64bit) >= set:kgJAZn6...`
        // and the matching provide is
        //   `libpcre2-8.so.0()(64bit) = set:kdafcrS9...`
        // — different `set:` payloads. The name index narrows to the
        // right capability; the EVR comparison treats both as Equal,
        // letting `Ge` succeed.
        let provide = EVR::new(Some(0), "set:kdafcrS9Ku7yZIO", "");
        let require = EVR::new(Some(0), "set:kgJAZn6CpJkW", "");
        let ord = provide.compare_rpm(&require);
        assert!(
            CapVersion::Ge(require.clone()).matches(ord),
            "Ge constraint with set: on both sides must be satisfied"
        );
        assert!(
            CapVersion::Eq(require).matches(ord),
            "Eq constraint with set: on both sides must be satisfied"
        );
    }
}

#[cfg(test)]
mod dependency_tests {
    //! Contract tests for the [`Dependency`] newtype. The newtype
    //! deliberately omits `Deref<Target = Capability>`, so the read
    //! API has to be exercised through inherent forwarders; pin that
    //! shape here so a future "let's just add Deref" change is a
    //! test diff, not a silent regression.
    use super::*;

    fn cap(name: &str) -> Capability {
        Capability::unversioned(name)
    }

    #[test]
    fn from_and_new_construct_equal_values() {
        // Both constructors round-trip through the same `Arc<str>`
        // body, so structural equality holds.
        let a = Dependency::new(cap("bash"));
        let b: Dependency = cap("bash").into();
        assert_eq!(a, b);
    }

    #[test]
    fn as_capability_round_trips_through_clone() {
        let dep = Dependency::unversioned("glibc");
        // `as_capability` borrows the inner value; constructing
        // a fresh `Dependency` from the clone must compare equal.
        let recovered = Dependency::new(dep.as_capability().clone());
        assert_eq!(dep, recovered);
    }

    #[test]
    fn name_version_forwarders_match_inner_capability() {
        // Read API: the inherent forwarders return the same
        // references the `Capability` would expose via its fields.
        let cap = Capability::ge("openssl", EVR::new(Some(0), "3.0.0", "1.el9"));
        let inner_name = cap.name.clone();
        let inner_version = cap.version.clone();
        let dep = Dependency::new(cap);
        assert_eq!(dep.name(), &inner_name);
        assert_eq!(dep.version(), &inner_version);
    }

    #[test]
    fn is_file_path_forwards() {
        let dep = Dependency::unversioned("/usr/bin/bash");
        assert!(dep.is_file_path());
        let dep = Dependency::unversioned("bash");
        assert!(!dep.is_file_path());
    }

    #[test]
    fn debug_format_matches_inner_capability() {
        // Manual `Debug` forwards to the wrapped `Capability`, so
        // `?dep` in tracing fields keeps producing the same shape
        // log shippers parsed before the newtype.
        let cap = Capability::unversioned("bash");
        let dep = Dependency::new(cap.clone());
        assert_eq!(format!("{dep:?}"), format!("{cap:?}"));
    }

    #[test]
    fn hashset_dedups_equal_dependencies() {
        // Hash + Eq derive on the tuple struct delegates to the
        // inner `Capability` impls — clones must collapse in a set.
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(Dependency::unversioned("bash"));
        set.insert(Dependency::unversioned("bash"));
        set.insert(Dependency::unversioned("glibc"));
        assert_eq!(set.len(), 2);
    }
}
