//! Repository configuration carried on a [`crate::Profile`].
//!
//! These types live in the `profile` crate (rather than in
//! `rpm-spec-repo-core`) so that `rpm-spec-profile` — which publishes
//! to crates.io — does not depend on the internal `repo-*` crates.
//! `rpm-spec-repo-core` re-exports them so backends and the resolver
//! can use the same type identities.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// All repositories plus buildroot configuration for one profile.
/// Stored on [`crate::Profile`] as `repos: Option<RepoSet>`; `None`
/// means the profile declares no repos and RPM-REPO-* lints skip.
#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RepoSet {
    pub repos: BTreeMap<String, RepoConfig>,
    pub buildroot: BuildrootConfig,
}

/// One `[profiles.X.repos.<id>]` block. Field names follow dnf's
/// convention so packagers recognise them. `id` (the TOML key) must
/// match `[a-z0-9_-]{1,64}`; validation lives at config-load time.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default, rename_all = "kebab-case")]
#[non_exhaustive]
pub struct RepoConfig {
    /// Base URL of the repository. Must end with `/`. Supports the
    /// dnf-style placeholders `$basearch`, `$arch`, `$releasever`,
    /// `$infra` interpolated from the profile's ArchInfo/Identity at
    /// load time. Required when `enabled = true`.
    pub baseurl: Option<String>,

    /// Metadata format. `auto` sniffs by HEADing both `repodata/`
    /// (rpm-md) and `base/` (apt-rpm).
    pub kind: RepoKind,

    /// Whether the resolver should consider this repo. Setting to
    /// `false` on an inherited (from `extends`) repo masks it without
    /// redefining the URL.
    pub enabled: bool,

    /// Resolver priority — lower wins on tie, matching dnf's
    /// convention. Default 99.
    pub priority: i32,

    /// Tier label used by lints (`role = "internal"` triggers
    /// shadowing checks) and `repo health` reports. Not load-bearing
    /// for the solver itself.
    pub role: RepoRole,

    /// GPG keys: file paths relative to the user's `.rpmspec.toml`,
    /// or `builtin:NAME` referencing a key bundled with the tool.
    pub gpgkey: Vec<String>,

    /// In P0: warn-only. In P1 (PR 14): hard enforce, override with
    /// `--insecure-skip-gpg`.
    pub gpgcheck: bool,

    /// apt-rpm only: which component (`classic`, `gostcrypto`, ...).
    /// For rpm-md this is derived from primary.xml automatically.
    pub components: Vec<String>,
}

impl Default for RepoConfig {
    fn default() -> Self {
        Self {
            baseurl: None,
            kind: RepoKind::Auto,
            enabled: true,
            priority: 99,
            role: RepoRole::default(),
            gpgkey: Vec::new(),
            gpgcheck: false,
            components: Vec::new(),
        }
    }
}

/// Which metadata format a repo serves. `Auto` lets the backend sniff
/// by HEAD-ing well-known paths on first fetch.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum RepoKind {
    #[default]
    Auto,
    RpmMd,
    AptRpm,
}

impl RepoKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::RpmMd => "rpm-md",
            Self::AptRpm => "apt-rpm",
        }
    }
}

/// Coarse tier classification of a configured repo. Affects which
/// repo-health and reverse-dep heuristics fire; never affects which
/// packages the resolver considers.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum RepoRole {
    #[default]
    Base,
    Updates,
    Product,
    Internal,
    Optional,
    Source,
    Debug,
}

/// `[profiles.X.buildroot]` block — what's installed in the chroot
/// before BuildRequires processing. Mirrors mock/koji/hasher base sets.
#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default, rename_all = "kebab-case")]
#[non_exhaustive]
pub struct BuildrootConfig {
    /// Packages assumed installed before any `BuildRequires` is
    /// processed. Entries are bare names; resolver pins each to the
    /// latest matching package in the configured repo set.
    pub base_packages: Vec<String>,

    /// "Shadow" BuildRequires that the platform's build system
    /// always provides implicitly (e.g. Fedora's `rpm-build`, `gcc`,
    /// `make`). Per-profile so RHEL / openSUSE / ALT can each declare
    /// their own conventions.
    pub implicit_buildrequires: Vec<String>,
}

/// Validate a repo identifier (TOML key under `[profiles.X.repos.<id>]`).
/// Returns the input unchanged on success or a descriptive error
/// suitable for surfacing in `ResolveError::Config`.
pub fn validate_repo_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("repo id is empty".into());
    }
    if id.len() > 64 {
        return Err(format!("repo id `{id}` exceeds 64 characters"));
    }
    if !id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
        return Err(format!(
            "repo id `{id}` must match `[a-z0-9_-]+` (lowercase ascii, digits, `-`, `_`)"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_repo_id_accepts_canonical() {
        for ok in ["baseos", "app-stream", "product_internal", "p11", "a"] {
            assert!(validate_repo_id(ok).is_ok(), "{ok} should be ok");
        }
    }

    #[test]
    fn validate_repo_id_rejects_bad() {
        for bad in ["", "BaseOS", "with space", "café", "x".repeat(65).as_str()] {
            assert!(validate_repo_id(bad).is_err(), "{bad} should be rejected");
        }
    }
}
