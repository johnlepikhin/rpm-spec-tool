//! updateinfo.xml parser. Skeleton in M1 — fully populates
//! [`Advisory`] in M5 when `repo security scan` ships.

use rpm_spec_repo_core::{Advisory, RepoError};

/// Parse updateinfo.xml. M1: returns empty vec on any input, so the
/// surrounding code keeps wiring without crashing on advisory-bearing
/// repos. Real parsing lands with `RPM-REPO-090` in M5.
pub fn parse(_xml: &[u8]) -> Result<Vec<Advisory>, RepoError> {
    Ok(Vec::new())
}
