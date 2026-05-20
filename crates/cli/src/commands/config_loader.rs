//! `rpmspec.toml` discovery and loading for non-cache CLI subcommands.
//!
//! Resolution order (matches [`crate::config::ConfigCache`] for spec-
//! lint commands — single source of truth per machine):
//!
//! 1. `--config <PATH>` on the CLI (explicit).
//! 2. `$RPM_SPEC_TOOL_CONFIG` env var (operator override for tests /
//!    one-off runs without retyping the long path).
//! 3. `$XDG_CONFIG_HOME/rpm-spec-tool/rpmspec.toml` (default; falls
//!    back to `~/.config/rpm-spec-tool/rpmspec.toml`).
//! 4. Built-in [`Config::default`] anchored at cwd.
//!
//! The pre-XDG version walked upward from cwd looking for
//! `.rpmspec.toml`. That made config discovery position-dependent
//! and surprising (same spec, different lint results depending on
//! where you cd'd to). XDG-only gives one rule per machine.
//!
//! Used by every subcommand that resolves profiles or target sets
//! *without* needing the spec-level `ConfigCache` (e.g. `profile`,
//! `target`, `matrix`, `repo`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use rpm_spec_analyzer::config::Config;
use rpm_spec_analyzer::config_cache::default_config_path;

/// Environment-variable override for the XDG-resolved config path.
/// Set to an absolute path; takes precedence over the XDG default
/// but loses to `--config <PATH>` on the CLI.
const ENV_CONFIG_OVERRIDE: &str = "RPM_SPEC_TOOL_CONFIG";

/// Resolve the config to use and return it paired with the directory
/// it was loaded from (the base for relative `showrc-file` paths).
///
/// Resolution order (highest priority first):
/// 1. `explicit` argument (CLI `--config <PATH>`).
/// 2. `$RPM_SPEC_TOOL_CONFIG`.
/// 3. `$XDG_CONFIG_HOME/rpm-spec-tool/rpmspec.toml`.
/// 4. [`Config::default`] anchored at the current working directory.
///
/// A missing XDG file is NOT an error — the tool's built-in profiles
/// still work without project config. An explicitly-named file that
/// doesn't exist IS an error (the operator asked for that file).
///
/// # Errors
///
/// Returns an error when an explicit / env-overridden config file
/// exists but can't be read or doesn't deserialize as TOML, or when
/// `current_dir()` fails in the default-fallback case.
pub fn load_config(explicit: Option<&Path>) -> Result<(Config, PathBuf)> {
    if let Some(path) = explicit {
        return load_from(path);
    }
    if let Ok(env_path) = std::env::var(ENV_CONFIG_OVERRIDE) {
        let path = PathBuf::from(env_path);
        return load_from(&path);
    }
    if let Some(xdg) = default_config_path()
        && xdg.is_file()
    {
        tracing::debug!(path = %xdg.display(), "loaded XDG-default config");
        return load_from(&xdg);
    }
    // No config available — degrade to built-in defaults anchored at
    // cwd. Lints still run against the built-in profile registry.
    let cwd = std::env::current_dir().context("getting current directory")?;
    tracing::debug!(
        cwd = %cwd.display(),
        "no rpmspec.toml found (explicit / env / XDG); using Config::default()"
    );
    Ok((Config::default(), cwd))
}

fn load_from(path: &Path) -> Result<(Config, PathBuf)> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let cfg =
        Config::from_toml_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let base = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    Ok((cfg, base))
}
