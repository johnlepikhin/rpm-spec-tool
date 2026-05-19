//! `RepoBackend` trait — the surface a backend (rpm-md, apt-rpm)
//! exposes to the rest of the tool. Stubbed in M1 PR1 skeleton; the
//! rpm-md implementation lands in this same PR before merge.

use rpm_spec_repo_core::{RepoConfig, RepoError, RepoId, RepoIndex, RepoKind, RepoRevision};

use crate::http::HttpCache;

/// Backend trait. All methods are synchronous and may block on
/// network and disk I/O. Implementations are constructed by
/// [`detect_backend`] and stored as `Box<dyn RepoBackend>` in the
/// session layer.
pub trait RepoBackend: Send + Sync + std::fmt::Debug {
    fn kind(&self) -> RepoKind;

    /// Fetch the top-level metadata file (`repomd.xml` or
    /// `base/release`) and derive a snapshot identity.
    fn fetch_revision(
        &self,
        http: &HttpCache,
        repo: &RepoConfig,
    ) -> Result<RepoRevision, RepoError>;

    /// Fetch + parse the full metadata into a populated
    /// [`RepoIndex`]. Per the M1 decision filelists / contents_index
    /// / srclist are loaded eagerly here — no lazy split.
    ///
    /// `repo_id` is conventionally the TOML key naming the repo so
    /// the resulting [`RepoIndex`] (and every embedded
    /// [`rpm_spec_repo_core::Package`]) carries the human-readable
    /// identifier the rest of the pipeline keys on. Fall back to the
    /// baseurl when no logical id is available.
    fn fetch_index(
        &self,
        http: &HttpCache,
        repo: &RepoConfig,
        rev: &RepoRevision,
        repo_id: &RepoId,
    ) -> Result<RepoIndex, RepoError>;
}

/// Pick a backend for the given config. `Auto` sniffs by HEADing
/// well-known paths.
pub fn detect_backend(repo: &RepoConfig) -> Result<Box<dyn RepoBackend>, RepoError> {
    match repo.kind {
        #[cfg(feature = "rpm-md")]
        RepoKind::RpmMd => Ok(Box::new(crate::rpmmd::RpmMdBackend)),

        #[cfg(feature = "apt-rpm")]
        RepoKind::AptRpm => Ok(Box::new(crate::aptrpm::AptRpmBackend)),

        // Auto-sniff is not implemented yet. `RepoKind::Auto` is the
        // `RepoConfig::default()` value, so silently falling back to
        // rpm-md would yield cryptic HTTP 404s for apt-rpm users who
        // forgot to set `kind` explicitly. Fail loudly with guidance.
        RepoKind::Auto => Err(RepoError::UnsupportedKind(
            "automatic backend detection is not yet implemented; set `kind = \"rpm-md\"` or `kind = \"apt-rpm\"` explicitly in `[profiles.X.repos.<id>]`".into(),
        )),

        // `RepoKind` is `#[non_exhaustive]` in `rpm-spec-profile`, so
        // this wildcard is always *reachable* from the compiler's
        // perspective even when every current variant has an explicit
        // arm. Using `#[expect(unreachable_patterns)]` would itself
        // warn (the lint never fires), so keep an `#[allow]` to
        // document the intent: future `RepoKind` variants land here
        // until an explicit arm is added.
        #[allow(unreachable_patterns)]
        other => Err(RepoError::UnsupportedKind(other.as_str().to_string())),
    }
}
