//! Shared `.rpmspec.toml` discovery and loading.
//!
//! Used by every subcommand that resolves profiles or target sets:
//! `profile`, `target`, `matrix`. The single discovery rule
//! (`--config <path>` first, otherwise walk up from CWD) keeps the
//! tool's behaviour predictable across commands.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use rpm_spec_analyzer::config::Config;

/// Load the config and return both it and the directory it was found
/// in (the base for relative `showrc-file` paths during resolution).
///
/// When `explicit` is `Some`, that exact file is read; when `None`,
/// walks up from CWD looking for `.rpmspec.toml`. A missing config
/// returns `Config::default()` anchored at CWD — not an error,
/// because the tool works against built-in profiles even without
/// project config.
pub fn load_config(explicit: Option<&Path>) -> Result<(Config, PathBuf)> {
    if let Some(path) = explicit {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let cfg =
            Config::from_toml_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let base = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        return Ok((cfg, base));
    }
    let cwd = std::env::current_dir().context("getting current directory")?;
    let mut dir = cwd.clone();
    loop {
        let candidate = dir.join(".rpmspec.toml");
        if candidate.is_file() {
            tracing::debug!(path = %candidate.display(), "found .rpmspec.toml");
            let text = std::fs::read_to_string(&candidate)
                .with_context(|| format!("reading {}", candidate.display()))?;
            let cfg = Config::from_toml_str(&text)
                .with_context(|| format!("parsing {}", candidate.display()))?;
            return Ok((cfg, dir));
        }
        if !dir.pop() {
            tracing::debug!(
                cwd = %cwd.display(),
                "no .rpmspec.toml found while walking up; using Config::default()"
            );
            return Ok((Config::default(), cwd));
        }
    }
}
