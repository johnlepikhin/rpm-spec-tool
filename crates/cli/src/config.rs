//! `rpmspec.toml` loading for CLI commands.
//!
//! The discovery + memoisation logic lives in
//! [`rpm_spec_analyzer::config_cache::ConfigCache`] so both the CLI and
//! the LSP server share it. This module just re-exports the cache type
//! and adds CLI-flavoured helpers:
//!
//! * [`make_config_cache`] — resolves `--config` / env / XDG cascade
//!   into a `ConfigCache` instance ready to hand to a batch command.
//! * [`ConfigCacheCliExt`] — `load_*_or_report` helpers that print
//!   errors to stderr instead of bubbling them up (fail-soft batch
//!   behaviour the LSP server doesn't want).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rpm_spec_analyzer::config::Config;
pub use rpm_spec_analyzer::config_cache::ConfigCache;
use rpm_spec_analyzer::config_cache::default_config_path;
use rpm_spec_analyzer::error_format::format_error_chain;

/// Environment-variable override for the XDG-resolved config path.
/// Mirrors the one [`crate::commands::config_loader::load_config`]
/// uses so both paths agree on which file gets loaded.
const ENV_CONFIG_OVERRIDE: &str = "RPM_SPEC_TOOL_CONFIG";

/// Build a `ConfigCache` honouring the same `--config` / env / XDG
/// cascade as [`crate::commands::config_loader::load_config`]:
///
/// 1. `explicit` argument (CLI `--config <PATH>`).
/// 2. `$RPM_SPEC_TOOL_CONFIG` env var.
/// 3. `$XDG_CONFIG_HOME/rpm-spec-tool/rpmspec.toml`.
/// 4. Built-in defaults when none of the above exist.
///
/// Returns a cache that loads its file lazily on first
/// `load_for(...)` call — same fail-soft semantics every batch CLI
/// subcommand expects.
#[must_use]
pub fn make_config_cache(explicit: Option<PathBuf>) -> ConfigCache {
    if let Some(p) = explicit {
        return ConfigCache::new(Some(p));
    }
    if let Ok(env_path) = std::env::var(ENV_CONFIG_OVERRIDE) {
        return ConfigCache::new(Some(PathBuf::from(env_path)));
    }
    // XDG default — but only if the file actually exists. A missing
    // XDG file silently degrades to built-in defaults (the tool's
    // built-in profile set covers most users).
    if let Some(xdg) = default_config_path()
        && xdg.is_file()
    {
        return ConfigCache::new(Some(xdg));
    }
    ConfigCache::new(None)
}

/// CLI-flavoured wrappers around [`ConfigCache`] that emit error
/// reports to stderr and flip a shared `any_io_error` flag instead of
/// returning the error. Used by batch subcommands so one bad spec
/// doesn't abort processing of the rest.
pub trait ConfigCacheCliExt {
    fn load_or_report(
        &mut self,
        source_path: &Path,
        any_io_error: &mut bool,
    ) -> Option<Arc<Config>>;

    fn load_with_base_dir_or_report(
        &mut self,
        source_path: &Path,
        any_io_error: &mut bool,
    ) -> Option<(Arc<Config>, PathBuf)>;
}

impl ConfigCacheCliExt for ConfigCache {
    fn load_or_report(
        &mut self,
        source_path: &Path,
        any_io_error: &mut bool,
    ) -> Option<Arc<Config>> {
        match self.load_for(source_path) {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("error: {}", format_error_chain(&e));
                *any_io_error = true;
                None
            }
        }
    }

    fn load_with_base_dir_or_report(
        &mut self,
        source_path: &Path,
        any_io_error: &mut bool,
    ) -> Option<(Arc<Config>, PathBuf)> {
        match self.load_for_with_base_dir(source_path) {
            Ok(pair) => Some(pair),
            Err(e) => {
                eprintln!("error: {}", format_error_chain(&e));
                *any_io_error = true;
                None
            }
        }
    }
}
