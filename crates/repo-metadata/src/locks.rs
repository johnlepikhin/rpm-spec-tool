//! Exclusive per-repo lock for concurrent `repo sync` invocations.
//!
//! Two parallel `rpm-spec-tool repo sync` of the same URL must
//! serialise so neither corrupts a partially-written snapshot. Reads
//! are concurrent (no lock).

use std::fs::{File, OpenOptions};
use std::path::Path;

use rustix::fs::{FlockOperation, flock};

use rpm_spec_repo_core::RepoError;

/// RAII guard: holds an fcntl `LOCK_EX` until dropped.
#[derive(Debug)]
pub struct RepoLockGuard {
    _file: File,
}

/// Acquire an exclusive lock on `<repo_dir>/.lock`. Blocks until
/// other writers release. Returns the guard; drop releases the lock.
pub fn acquire(repo_dir: &Path) -> Result<RepoLockGuard, RepoError> {
    std::fs::create_dir_all(repo_dir)?;
    let path = repo_dir.join(".lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)?;
    // rustix::io::Errno: Into<std::io::Error> preserves the underlying
    // errno (e.g. EINTR, ENOLCK) so callers can match programmatically
    // instead of grepping a formatted string.
    flock(&file, FlockOperation::LockExclusive)
        .map_err(|e| RepoError::Cache(std::io::Error::from(e)))?;
    Ok(RepoLockGuard { _file: file })
}
