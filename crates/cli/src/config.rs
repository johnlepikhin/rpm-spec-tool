//! `.rpmspec.toml` loading for CLI commands.
//!
//! The discovery + memoisation logic lives in
//! [`rpm_spec_analyzer::config_cache::ConfigCache`] so both the CLI and
//! the LSP server share it. This module just re-exports the cache type
//! and adds CLI-flavoured `load_*_or_report` helpers that print errors
//! to stderr instead of bubbling them up — fail-soft batch behaviour
//! the LSP server doesn't want.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rpm_spec_analyzer::config::Config;
pub use rpm_spec_analyzer::config_cache::ConfigCache;
use rpm_spec_analyzer::error_format::format_error_chain;

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
