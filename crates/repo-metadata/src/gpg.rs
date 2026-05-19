//! GPG verification stub for repomd / package signatures.
//!
//! M1 / P0 policy: warn-only. The CLI surface ships the `gpgcheck`
//! TOML field, but verification is deferred to PR 14 (M4) when the
//! hard-enforce path lands together with `--insecure-skip-gpg` and
//! the bundled key catalog.

use rpm_spec_repo_core::RepoError;

/// Placeholder: never errors in M1, but the function is here so the
/// call site exists before PR 14 wires the real verifier.
pub fn verify_detached_signature(
    _signed_bytes: &[u8],
    _signature_bytes: &[u8],
    _key_ids: &[String],
) -> Result<(), RepoError> {
    tracing::trace!("gpg verification is currently a warn-only stub");
    Ok(())
}
