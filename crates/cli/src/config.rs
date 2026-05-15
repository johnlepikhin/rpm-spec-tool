//! `.rpmspec.toml` loading.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rpm_spec_analyzer::config::Config;

/// Load a config from an explicit path or by walking upward from a source
/// file. Falls back to defaults if no config is found.
pub fn load(explicit: Option<&Path>, near: Option<&Path>) -> Result<Config> {
    if let Some(path) = explicit {
        return load_from(path);
    }
    if let Some(start) = near
        && let Some(found) = discover_upward(start)
    {
        return load_from(&found);
    }
    Ok(Config::default())
}

fn load_from(path: &Path) -> Result<Config> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read config {}", path.display()))?;
    Config::from_toml_str(&text)
        .with_context(|| format!("failed to parse config {}", path.display()))
}

fn discover_upward(start: &Path) -> Option<PathBuf> {
    let mut dir = if start.is_file() {
        start.parent()?.to_path_buf()
    } else {
        start.to_path_buf()
    };
    loop {
        let candidate = dir.join(".rpmspec.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}
