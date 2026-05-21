//! `repo` subcommand — manage the on-disk metadata cache that backs
//! `matrix deps check` and friends.
//!
//! Modes (M1):
//! * `repo sync` — fetch metadata for one or more profiles. Only
//!   command in the tool that touches the network.
//! * `repo cache {ls, gc, prune}` — inspect / manage the on-disk
//!   cache.
//! * `repo show` — summarise a fetched repository (revision,
//!   package count, top packages by installed size).
//!
//! Future modes (M2+): `repo health`, `repo lock`, `repo impact`,
//! `repo security scan`.
//!
//! ## Exit codes
//!
//! * `0` — success.
//! * `1` — fetch / parse / cache I/O failure.
//! * `2` — soft user error (unknown profile, invalid flags).
//! * `3` — infra error (no network in `Online` mode, …).

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, Subcommand};

mod cache;
mod show;
mod status;
mod sync;
pub(crate) mod url;

pub use cache::CacheOpts;
pub use show::ShowOpts;
pub use status::StatusOpts;
pub use sync::SyncOpts;

/// Fallback profile name used when the user hasn't set one in
/// `.rpmspec.toml` and didn't pass `--profile`. Re-export of
/// [`rpm_spec_profile::builtin::DEFAULT_BUILTIN`] so the two
/// strings can't drift.
pub(super) const DEFAULT_PROFILE_NAME: &str = rpm_spec_analyzer::profile::builtin::DEFAULT_BUILTIN;

/// Shared `repo` args. Almost every action accepts the same network
/// / cache flags so they're flattened here.
#[derive(Debug, Args, Clone)]
pub struct RepoArgs {
    /// Allow network fetches. Without this flag every action runs
    /// in cache-only mode and errors on cache miss.
    #[arg(long, global = true)]
    pub allow_fetch: bool,

    /// Force offline mode even if the action's default is `Online`.
    #[arg(long, global = true, conflicts_with = "allow_fetch")]
    pub offline: bool,

    /// Cache-only: same as offline but errors on cache miss.
    /// Recommended for CI invocations.
    #[arg(long, global = true, conflicts_with_all = ["allow_fetch", "offline"])]
    pub cache_only: bool,

    /// Override the cache directory (default:
    /// `$XDG_CACHE_HOME/rpm-spec-tool/`, or
    /// `$RPM_SPEC_TOOL_CACHE_DIR` env var).
    #[arg(long, global = true, value_name = "DIR")]
    pub cache_dir: Option<PathBuf>,

    /// Disable TLS certificate verification for HTTPS fetches.
    ///
    /// SECURITY: trusts ANY server identity — MITM, DNS hijack, and
    /// strip-and-replay all succeed silently. Use only when the
    /// corporate mirror's CA is missing from the system trust store
    /// and the network path is otherwise trusted. Production CI must
    /// install the CA via `update-ca-trust` / `update-ca-certificates`
    /// instead of using this flag.
    #[arg(long, global = true)]
    pub insecure_tls: bool,
}

impl RepoArgs {
    /// Resolve the requested network mode for an action whose
    /// default is `default_mode` (typically `Offline` for read
    /// actions and `Online` for `sync`).
    pub fn resolve_mode(
        &self,
        default_mode: rpm_spec_repo_metadata::http::NetMode,
    ) -> rpm_spec_repo_metadata::http::NetMode {
        use rpm_spec_repo_metadata::http::NetMode;
        if self.cache_only {
            return NetMode::CacheOnly;
        }
        if self.offline {
            return NetMode::Offline;
        }
        if self.allow_fetch {
            return NetMode::Online;
        }
        default_mode
    }

    /// Resolve the cache root path. CLI flag wins over env var which
    /// wins over `directories::ProjectDirs` default.
    pub fn resolve_cache_root(&self) -> Result<PathBuf> {
        if let Some(p) = &self.cache_dir {
            std::fs::create_dir_all(p)?;
            return Ok(p.clone());
        }
        Ok(rpm_spec_repo_metadata::cache::default_cache_root()?)
    }
}

#[derive(Debug, Args)]
pub struct Cmd {
    /// Explicit path to `.rpmspec.toml`. Walks up from CWD when
    /// unset.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    #[command(flatten)]
    pub repo_args: RepoArgs,

    #[command(subcommand)]
    pub action: Action,
}

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Fetch repository metadata for one or more profiles.
    Sync(SyncOpts),
    /// Inspect a fetched repository.
    Show(ShowOpts),
    /// Quick health check: per-repo sync status for a profile.
    Status(StatusOpts),
    /// Manage the on-disk cache (list / gc / prune).
    Cache(CacheOpts),
}

impl Cmd {
    pub fn run(self, color: crate::app::ColorChoice) -> Result<ExitCode> {
        let (config, base_dir) =
            crate::commands::config_loader::load_config(self.config.as_deref())?;
        let style = crate::commands::profile::style::Style::new(color);
        match self.action {
            Action::Sync(opts) => sync::run(opts, self.repo_args, &config, &base_dir, &style),
            Action::Show(opts) => show::run(opts, self.repo_args, &config, &base_dir, &style),
            Action::Status(opts) => status::run(opts, self.repo_args, &config, &base_dir, &style),
            Action::Cache(opts) => cache::run(opts, self.repo_args, &config, &base_dir, &style),
        }
    }
}
