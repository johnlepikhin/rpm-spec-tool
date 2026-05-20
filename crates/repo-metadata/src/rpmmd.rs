//! rpm-md backend: parses `repodata/repomd.xml` and the referenced
//! `primary.xml.(gz|zst)`, `filelists.xml.(gz|zst)`, and
//! `updateinfo.xml.gz` into a [`RepoIndex`].
//!
//! Per the M1 decision filelists are loaded eagerly during
//! [`fetch_index`] so file-owner lookups in lints don't ever need a
//! second fetch. updateinfo is parsed when present and skipped
//! otherwise.

mod filelists;
mod primary;
mod repomd;
mod updateinfo;

use time::OffsetDateTime;

use rpm_spec_repo_core::{RepoError, RepoId, RepoIndex, RepoKind, RepoRevision};

use crate::backend::RepoBackend;
use crate::http::HttpCache;

/// Stateless backend: parsers are pure functions, the backend struct
/// is a discriminator only.
#[derive(Debug, Default)]
pub struct RpmMdBackend;

impl RepoBackend for RpmMdBackend {
    fn kind(&self) -> RepoKind {
        RepoKind::RpmMd
    }

    fn fetch_revision(
        &self,
        http: &HttpCache,
        baseurl: &str,
    ) -> Result<RepoRevision, RepoError> {
        let url = join_url(baseurl, "repodata/repomd.xml");
        let bytes = http.fetch(&url)?;
        let revision = crate::cache::revision_from(&bytes);
        Ok(RepoRevision {
            id: revision,
            timestamp: OffsetDateTime::now_utc(),
            raw_bytes: bytes,
        })
    }

    fn fetch_index(
        &self,
        http: &HttpCache,
        baseurl: &str,
        rev: &RepoRevision,
        repo_id: &RepoId,
    ) -> Result<RepoIndex, RepoError> {
        let repomd = repomd::parse(&rev.raw_bytes)?;

        let mut index = RepoIndex {
            repo_id: repo_id.clone(),
            revision: rev.id.clone(),
            fetched_at: rev.timestamp,
            packages: Vec::new(),
            advisories: Vec::new(),
        };

        if let Some(primary_loc) = repomd.location_for("primary") {
            let url = join_url(baseurl, &primary_loc);
            let raw = http.fetch(&url)?;
            let xml = crate::compression::decompress(&primary_loc, &raw)?;
            index.packages = primary::parse(&xml, repo_id.clone())?;
        }

        if let Some(filelists_loc) = repomd.location_for("filelists") {
            let url = join_url(baseurl, &filelists_loc);
            let raw = http.fetch(&url)?;
            let xml = crate::compression::decompress(&filelists_loc, &raw)?;
            filelists::merge(&xml, &mut index.packages)?;
        }

        if let Some(ui_loc) = repomd.location_for("updateinfo") {
            let url = join_url(baseurl, &ui_loc);
            let raw = http.fetch(&url)?;
            let xml = crate::compression::decompress(&ui_loc, &raw)?;
            index.advisories = updateinfo::parse(&xml)?;
        }

        Ok(index)
    }
}

fn join_url(base: &str, rel: &str) -> String {
    let base = base.trim_end_matches('/');
    let rel = rel.trim_start_matches('/');
    format!("{base}/{rel}")
}
