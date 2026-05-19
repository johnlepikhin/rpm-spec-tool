//! On-disk snapshot cache under `~/.cache/rpm-spec-tool/repos/`.
//!
//! Layout (see `doc/repos.md`):
//! ```text
//! repos/<sha256-canonical-baseurl>/
//!   current -> snapshots/<rev>/
//!   snapshots/<rev>/
//!     repomd.xml | release
//!     primary.xml, filelists.xml, updateinfo.xml (decompressed)
//!     index.bincode             # fast-reload parsed RepoIndex
//!     manifest.json             # backend kind, fetched_at, sha, bytes
//!   revisions.log
//! ```
//!
//! Atomic snapshot writes: each fresh snapshot is materialised in
//! `tmp/`, fsync'd, then renamed under `snapshots/<rev>/` and the
//! `current` symlink is repointed. Concurrent writers serialise on a
//! per-repo fcntl lock acquired via [`crate::locks`].

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use rpm_spec_repo_core::db::RepoDb;
use rpm_spec_repo_core::{RepoError, RepoIndex, RepoKind};

use crate::util::{atomic_write, sha256_hex};

/// Bumped when [`SnapshotManifest`] / serialised RepoIndex layout
/// changes. Cache hits with a different version are evicted and
/// re-fetched.
pub const CACHE_SCHEMA_VERSION: u32 = 1;

/// Per-snapshot upper bound on bincode deserialisation. 4 GiB
/// covers any realistic Fedora-scale `RepoIndex` (~60k packages × a
/// few KB of metadata each) but caps allocation when a corrupt
/// `index.bincode` claims absurd lengths. Matches the
/// `compression::MAX_DECOMPRESSED_BYTES` ceiling.
pub const BINCODE_DESERIALIZE_LIMIT: u64 = 4 * 1024 * 1024 * 1024;

/// Per-snapshot manifest committed next to the parsed index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotManifest {
    pub schema_version: u32,
    pub backend_kind: String,
    pub revision: String,
    #[serde(with = "time::serde::rfc3339")]
    pub fetched_at: OffsetDateTime,
    pub bytes_fetched: u64,
    pub baseurl_sha256: String,
}

/// Locate the cache root, defaulting to `~/.cache/rpm-spec-tool/` via
/// `directories::ProjectDirs`. Override with `RPM_SPEC_TOOL_CACHE_DIR`
/// or the `--cache-dir` CLI flag (CLI passes the resolved path
/// directly).
pub fn default_cache_root() -> Result<PathBuf, RepoError> {
    if let Ok(v) = std::env::var("RPM_SPEC_TOOL_CACHE_DIR") {
        let p = PathBuf::from(v);
        fs::create_dir_all(&p)?;
        return Ok(p);
    }
    let dirs = directories::ProjectDirs::from("io", "rpm-spec-tool", "rpm-spec-tool")
        .ok_or_else(|| RepoError::Config("could not determine project cache directory".into()))?;
    let p = dirs.cache_dir().to_path_buf();
    fs::create_dir_all(&p)?;
    Ok(p)
}

/// Canonical key for a repo on disk: sha256 of the canonicalised
/// baseurl. Different profiles pointing at the same URL share one
/// snapshot directory (auto-dedup per the design).
#[must_use]
pub fn baseurl_key(baseurl: &str) -> String {
    let canonical = canonicalise_url(baseurl);
    sha256_hex(canonical.as_bytes())
}

fn canonicalise_url(u: &str) -> String {
    // Cheap canonicalisation: strip trailing slashes (we tolerate
    // either form in config) and lowercase the scheme + host. We
    // explicitly do NOT do query-string sorting or punycode — repo
    // URLs are stable and human-curated.
    let trimmed = u.trim_end_matches('/');
    if let Some(idx) = trimmed.find("://") {
        let (scheme, rest) = trimmed.split_at(idx);
        let rest = &rest[3..];
        let mut out = String::with_capacity(trimmed.len() + 3);
        out.push_str(&scheme.to_ascii_lowercase());
        out.push_str("://");
        if let Some(slash) = rest.find('/') {
            out.push_str(&rest[..slash].to_ascii_lowercase());
            out.push_str(&rest[slash..]);
        } else {
            out.push_str(&rest.to_ascii_lowercase());
        }
        out
    } else {
        trimmed.to_string()
    }
}

/// Compute a snapshot revision id (sha256 hex of the format-defining
/// file: `repomd.xml` for rpm-md, `base/release` for apt-rpm).
#[must_use]
pub fn revision_from(bytes: &[u8]) -> String {
    sha256_hex(bytes)
}

/// Directory layout helper.
#[derive(Debug, Clone)]
pub struct CacheDirs {
    pub root: PathBuf,
    pub repos: PathBuf,
    pub tmp: PathBuf,
    pub lockfiles_registry: PathBuf,
}

impl CacheDirs {
    pub fn ensure(root: PathBuf) -> Result<Self, RepoError> {
        let repos = root.join("repos");
        let tmp = root.join("tmp");
        let lockfiles_registry = root.join("lockfiles.json");
        fs::create_dir_all(&repos)?;
        fs::create_dir_all(&tmp)?;
        write_version_marker(&root)?;
        Ok(Self {
            root,
            repos,
            tmp,
            lockfiles_registry,
        })
    }

    pub fn repo_dir(&self, baseurl: &str) -> PathBuf {
        self.repos.join(baseurl_key(baseurl))
    }

    pub fn snapshot_dir(&self, baseurl: &str, revision: &str) -> PathBuf {
        self.repo_dir(baseurl).join("snapshots").join(revision)
    }
}

fn write_version_marker(root: &Path) -> std::io::Result<()> {
    let path = root.join("version");
    if !path.exists() {
        fs::write(&path, CACHE_SCHEMA_VERSION.to_string())?;
    }
    Ok(())
}

/// Persist a parsed [`RepoIndex`] under the snapshot directory,
/// emitting both `index.bincode` and `manifest.json` atomically.
///
/// Raw metadata files (`repomd.xml`, `primary.xml.gz`, …) are NOT
/// currently staged alongside; they live in the HTTP cache keyed
/// by URL. Future work may copy them into the snapshot for
/// reproducibility.
pub fn write_snapshot(
    dirs: &CacheDirs,
    baseurl: &str,
    backend_kind: RepoKind,
    index: &RepoIndex,
    bytes_fetched: u64,
) -> Result<PathBuf, RepoError> {
    let snap_dir = dirs.snapshot_dir(baseurl, &index.revision);
    fs::create_dir_all(&snap_dir)?;

    let manifest = SnapshotManifest {
        schema_version: CACHE_SCHEMA_VERSION,
        backend_kind: backend_kind.as_str().to_string(),
        revision: index.revision.clone(),
        fetched_at: index.fetched_at,
        bytes_fetched,
        baseurl_sha256: baseurl_key(baseurl),
    };

    let manifest_json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| RepoError::Serialize(e.to_string()))?;
    atomic_write(&snap_dir.join("manifest.json"), manifest_json.as_bytes())?;

    let bin = bincode::serialize(index).map_err(|e| RepoError::Serialize(e.to_string()))?;
    atomic_write(&snap_dir.join("index.bincode"), &bin)?;

    // Write the SQLite mirror next to the bincode snapshot. The DB
    // becomes the load-time source of truth once Phase 3 wires the
    // reader path, but we emit both in Phase 2 so an in-place
    // upgrade doesn't force a re-sync. Tmp-rename keeps the file
    // atomic from a reader's perspective; SQLite's own WAL handles
    // crash-during-write.
    write_repo_db(
        &snap_dir,
        baseurl,
        backend_kind,
        index,
    )?;

    // Repoint `current` to the new snapshot. ENOENT is benign (first
    // snapshot for this repo); any other error indicates a real
    // filesystem problem and must surface.
    let current = dirs.repo_dir(baseurl).join("current");
    match fs::remove_file(&current) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    std::os::unix::fs::symlink(&snap_dir, &current)?;

    Ok(snap_dir)
}

/// Build the per-snapshot `repo.db` from a parsed [`RepoIndex`].
///
/// The DB is the load-time backend that replaces `index.bincode`
/// (the old format is still written for downgrade safety in Phase 2;
/// readers swap over in Phase 3 / 4). Writing happens to a `.tmp`
/// path so a partial DB never appears under the snapshot name —
/// if the process dies mid-ingest the next run sees no `repo.db`
/// and re-parses XML.
fn write_repo_db(
    snap_dir: &Path,
    baseurl: &str,
    backend_kind: RepoKind,
    index: &RepoIndex,
) -> Result<(), RepoError> {
    let final_path = snap_dir.join(RepoDb::file_name());
    let tmp_path = snap_dir.join(format!("{}.tmp", RepoDb::file_name()));
    if tmp_path.exists() {
        fs::remove_file(&tmp_path)?;
    }

    let mut db = RepoDb::create(
        &tmp_path,
        &index.repo_id,
        &index.revision,
        index.fetched_at,
        backend_kind.as_str(),
        &baseurl_key(baseurl),
    )?;
    db.ingest_packages(&index.packages)?;
    drop(db);

    // Atomic publication: rename into place. SQLite's WAL sidecar
    // files (`*-wal`, `*-shm`) for the freshly created DB are
    // empty after the connection closed (commit + checkpoint), so
    // renaming just the main file is safe.
    fs::rename(&tmp_path, &final_path)?;
    Ok(())
}

/// Try loading a cached parsed index. Returns `Ok(None)` when no
/// matching snapshot is present; `Ok(Some(_))` on hit; `Err` on
/// corruption (caller may choose to re-parse from raw XML).
pub fn try_load_snapshot(
    dirs: &CacheDirs,
    baseurl: &str,
    revision: &str,
) -> Result<Option<RepoIndex>, RepoError> {
    let snap = dirs.snapshot_dir(baseurl, revision);
    let bin = snap.join("index.bincode");
    if !bin.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&bin)?;
    use bincode::Options;
    let options = bincode::DefaultOptions::new()
        .with_limit(BINCODE_DESERIALIZE_LIMIT)
        .with_fixint_encoding()
        .allow_trailing_bytes();
    match options.deserialize::<RepoIndex>(&bytes) {
        Ok(idx) => Ok(Some(idx)),
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %bin.display(),
                "bincode snapshot rejected (corrupt or oversized); re-parse from XML"
            );
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseurl_key_canonicalises_scheme_and_host_case() {
        // Different cases on scheme+host should produce the same
        // snapshot directory; the path is preserved verbatim.
        let a = baseurl_key("https://Repo.Example/path/");
        let b = baseurl_key("HTTPS://repo.example/path");
        let c = baseurl_key("https://repo.example/path/");
        assert_eq!(a, b, "case-insensitive scheme+host should canonicalise");
        assert_eq!(a, c, "trailing slash should be stripped");
    }

    #[test]
    fn baseurl_key_preserves_path_case() {
        let a = baseurl_key("https://repo.example/Path/");
        let b = baseurl_key("https://repo.example/path/");
        assert_ne!(a, b, "path case is significant (servers may differ)");
    }

    #[test]
    fn revision_from_is_sha256_hex() {
        let r = revision_from(b"hello");
        assert_eq!(r.len(), 64);
        assert!(r.chars().all(|c| c.is_ascii_hexdigit()));
    }
}

