//! apt-rpm backend: parses ALT Linux's `base/release` +
//! `base/pkglist.classic.xz` + `base/srclist.classic.xz` +
//! `base/contents_index` into a [`RepoIndex`].
//!
//! Layout per ALT Linux convention:
//!
//! ```text
//! <baseurl>/
//!   base/
//!     release                  # text metadata + MD5 table
//!     pkglist.classic.xz       # binary rpm headers, xz-compressed
//!     srclist.classic.xz       # binary rpm headers (SRPMs)
//!     contents_index           # text path<TAB>package map (uncompressed)
//!   RPMS.classic/              # actual .rpm bodies
//! ```
//!
//! The backend fetches all four files eagerly during
//! [`AptRpmBackend::fetch_index`] (per the M1 decision — no lazy
//! split between metadata and filelists). Total wire cost for a
//! full ALT branch is ~25–30 MB compressed; decompressed working
//! set ~300 MB at peak. The SQLite-backed [`RepoIndex`] then trims
//! this to ~150 MB on disk via the per-repo `repo.db`.

mod contents;
mod error;
mod header;
mod package;
mod release;

pub use contents::FileMap;
pub use release::{ChecksumEntry, ReleaseFile};

use std::sync::Arc;

use time::OffsetDateTime;

use rpm_spec_repo_core::{RepoError, RepoId, RepoIndex, RepoKind, RepoRevision};

use crate::backend::RepoBackend;
use crate::http::HttpCache;

pub use error::AptRpmParseError;

/// Standalone helper: take raw (decompressed) pkglist bytes and
/// return parsed [`rpm_spec_repo_core::Package`] values. Exposed so
/// integration tests and future tooling (e.g. a CLI debug command
/// that dumps a pkglist's contents) can drive the parser without
/// going through the full `HttpCache + baseurl + fetch_index`
/// stack.
///
/// The backend's [`AptRpmBackend::fetch_index`] uses the same
/// pair of `header::parse_chain` + `package::package_from_header`
/// — so the public test surface tracks the production parser
/// exactly.
///
/// # Errors
///
/// Surfaces [`AptRpmParseError`] from the underlying header chain
/// parser. A header that's syntactically valid but missing required
/// tags (NAME / VERSION / RELEASE / ARCH) is silently skipped — same
/// policy as production sync, so a single stub package doesn't
/// abort the whole repo ingest.
pub fn parse_pkglist_bytes(
    bytes: &[u8],
    repo_id: &RepoId,
) -> Result<Vec<rpm_spec_repo_core::Package>, AptRpmParseError> {
    let headers = header::parse_chain(bytes)?;
    let mut out = Vec::with_capacity(headers.len());
    for h in &headers {
        if let Some(p) = package::package_from_header(h, repo_id) {
            out.push(p);
        }
    }
    Ok(out)
}

/// Standalone helper for `base/release`. Mirrors
/// [`parse_pkglist_bytes`] — usable in tests and tooling without
/// the HTTP layer.
///
/// # Errors
///
/// See [`AptRpmParseError::BadReleaseFile`].
pub fn parse_release_bytes(bytes: &[u8]) -> Result<release::ReleaseFile, AptRpmParseError> {
    let text = std::str::from_utf8(bytes).map_err(|e| AptRpmParseError::BadReleaseFile {
        detail: format!("not UTF-8: {e}"),
    })?;
    release::parse(text)
}

/// Standalone helper for `base/contents_index`. Mirrors
/// [`parse_pkglist_bytes`] — usable in tests and tooling without
/// the HTTP layer.
///
/// # Errors
///
/// See [`AptRpmParseError::BadContentsIndexLine`].
pub fn parse_contents_index_bytes(bytes: &[u8]) -> Result<contents::FileMap, AptRpmParseError> {
    let text = std::str::from_utf8(bytes).map_err(|e| AptRpmParseError::BadContentsIndexLine {
        line: 0,
        detail: format!("not UTF-8: {e}"),
    })?;
    contents::parse(text)
}

/// Stateless backend: parsers are pure functions, the backend
/// struct is a discriminator only.
#[derive(Debug, Default)]
pub struct AptRpmBackend;

impl RepoBackend for AptRpmBackend {
    fn kind(&self) -> RepoKind {
        RepoKind::AptRpm
    }

    fn fetch_revision(&self, http: &HttpCache, baseurl: &str) -> Result<RepoRevision, RepoError> {
        let url = join_url(baseurl, "base/release");
        let bytes = http.fetch(&url)?;
        // Validate as a sanity-check — if the file isn't a real
        // release we fail fast at sync time rather than letting the
        // unparseable header chain detonate later inside fetch_index.
        let text = std::str::from_utf8(&bytes)
            .map_err(|e| RepoError::parse_at_file("base/release", format!("not UTF-8: {e}")))?;
        let _ = release::parse(text)?;
        // Snapshot id = sha256 of the whole release bytes. Same
        // policy as rpm-md (sha of repomd.xml). Two syncs of the
        // same upstream revision produce identical id → cache hit.
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
        let mut index = RepoIndex {
            repo_id: repo_id.clone(),
            revision: rev.id.clone(),
            fetched_at: rev.timestamp,
            packages: Vec::new(),
            advisories: Vec::new(),
        };

        // 1) Binary package list — the resolver's primary input.
        let pkglist_loc = "base/pkglist.classic.xz";
        let pkglist_url = join_url(baseurl, pkglist_loc);
        let pkglist_raw = http.fetch(&pkglist_url)?;
        let pkglist_bytes = crate::compression::decompress(pkglist_loc, &pkglist_raw)?;
        let headers = header::parse_chain(&pkglist_bytes)?;
        index.packages.reserve(headers.len());
        for h in &headers {
            if let Some(pkg) = package::package_from_header(h, repo_id) {
                index.packages.push(pkg);
            }
            // Skipping a header silently is the right policy here:
            // ALT mirrors occasionally ship "marker" headers without
            // NAME (for stub packages); these aren't installable and
            // would clutter the index with degenerate rows.
        }

        // 2) File ownership — merge contents_index into Package.files.
        // Best-effort: ALT mirrors don't always publish a
        // contents_index per-arch (some only at the branch root).
        // A 404 here doesn't abort the whole index; the resolver
        // simply won't have file-owner data for absolute-path BRs.
        let contents_loc = "base/contents_index";
        let contents_url = join_url(baseurl, contents_loc);
        match http.fetch(&contents_url) {
            Ok(contents_raw) => {
                let contents_text = std::str::from_utf8(&contents_raw).map_err(|e| {
                    RepoError::parse_at_file("contents_index", format!("not UTF-8: {e}"))
                })?;
                let file_map = contents::parse(contents_text)?;
                merge_files(&mut index.packages, &file_map);
            }
            Err(e) => {
                tracing::warn!(
                    url = %contents_url,
                    error = ?e,
                    "contents_index not fetched; file-path BR / file-conflict lints \
                     will have no data on this repo",
                );
            }
        }

        // 3) srclist — binary SRPM headers. Same wire format as
        // pkglist but the records carry SRC arch. Used by
        // upgrade-sim / RPM-REPO-030/031 to map a spec's `Name:`
        // back to its source-side identity. apt-rpm models source
        // packages with `arch = "src"`; we honour that so the
        // arch-aware universe filter doesn't accidentally include
        // SRPMs in binary closure queries.
        let srclist_loc = "base/srclist.classic.xz";
        let srclist_url = join_url(baseurl, srclist_loc);
        match http.fetch(&srclist_url) {
            Ok(srclist_raw) => {
                let srclist_bytes = crate::compression::decompress(srclist_loc, &srclist_raw)?;
                let src_headers = header::parse_chain(&srclist_bytes)?;
                for h in &src_headers {
                    if let Some(pkg) = package::package_from_header(h, repo_id) {
                        index.packages.push(pkg);
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    url = %srclist_url,
                    error = ?e,
                    "srclist not fetched; upgrade-sim source-name lookups will be \
                     incomplete on this repo",
                );
            }
        }

        Ok(index)
    }
}

/// Splice each package's filelist from the `contents_index` map
/// into its `Package.files` vec. Best-effort: a package present in
/// pkglist but absent from contents_index keeps its empty files
/// list (file-owner queries for its paths will miss — acceptable
/// degradation rather than failing the whole sync).
fn merge_files(packages: &mut [rpm_spec_repo_core::Package], file_map: &contents::FileMap) {
    for pkg in packages {
        let key: Arc<str> = pkg.nevra.name.clone();
        if let Some(files) = file_map.get(&key) {
            pkg.files = files.clone();
        }
    }
}

fn join_url(base: &str, rel: &str) -> String {
    let base = base.trim_end_matches('/');
    let rel = rel.trim_start_matches('/');
    format!("{base}/{rel}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_url_normalises_slashes() {
        assert_eq!(
            join_url("http://x.example/p10/branch/x86_64", "base/release"),
            "http://x.example/p10/branch/x86_64/base/release"
        );
        assert_eq!(
            join_url("http://x.example/p10/branch/x86_64/", "/base/release"),
            "http://x.example/p10/branch/x86_64/base/release"
        );
    }

    #[test]
    fn merge_files_attaches_paths_by_name() {
        use rpm_spec_repo_core::{Capability, NEVRA, Package, PkgChecksum, RepoId};
        let mut packages = vec![Package {
            nevra: NEVRA {
                name: Arc::from("bash"),
                epoch: 0,
                version: Arc::from("5"),
                release: Arc::from("1"),
                arch: Arc::from("x86_64"),
            },
            repo_id: RepoId::from("classic"),
            provides: Vec::new(),
            requires: Vec::new(),
            conflicts: Vec::new(),
            obsoletes: Vec::new(),
            recommends: Vec::new(),
            suggests: Vec::new(),
            supplements: Vec::new(),
            enhances: Vec::new(),
            source_rpm: None,
            summary: Arc::from(""),
            size_installed: 0,
            checksum: PkgChecksum::Other {
                algo: "none".into(),
                hex: String::new(),
            },
            location: Arc::from(""),
            files: Vec::new(),
        }];
        let _: Vec<Capability> = Vec::new(); // silence unused import in test
        let mut map: contents::FileMap = std::collections::HashMap::new();
        map.insert(
            Arc::from("bash"),
            vec![Arc::from("/bin/sh"), Arc::from("/bin/bash")],
        );
        merge_files(&mut packages, &map);
        assert_eq!(packages[0].files.len(), 2);
        assert_eq!(packages[0].files[0].as_ref(), "/bin/sh");
    }
}
