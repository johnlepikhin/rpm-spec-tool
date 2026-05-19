//! apt-rpm backend stub.
//!
//! M1 ships a wired-but-non-functional backend so the feature can be
//! enabled in default builds without a separate `--no-default-features`
//! step. Returns `RepoError::UnsupportedKind` from every method.
//!
//! The real parsers (pkglist.classic.xz, srclist.classic.xz,
//! base/release, contents_index) ship in M3 (PR 6).

use rpm_spec_repo_core::{RepoConfig, RepoError, RepoId, RepoIndex, RepoKind, RepoRevision};

use crate::backend::RepoBackend;
use crate::http::HttpCache;

#[derive(Debug, Default)]
pub struct AptRpmBackend;

impl RepoBackend for AptRpmBackend {
    fn kind(&self) -> RepoKind {
        RepoKind::AptRpm
    }

    fn fetch_revision(&self, _http: &HttpCache, _repo: &RepoConfig) -> Result<RepoRevision, RepoError> {
        Err(RepoError::UnsupportedKind(
            "apt-rpm backend not yet implemented; tracked in the project roadmap".into(),
        ))
    }

    fn fetch_index(
        &self,
        _http: &HttpCache,
        _repo: &RepoConfig,
        _rev: &RepoRevision,
        _repo_id: &RepoId,
    ) -> Result<RepoIndex, RepoError> {
        Err(RepoError::UnsupportedKind(
            "apt-rpm backend not yet implemented; tracked in the project roadmap".into(),
        ))
    }
}
