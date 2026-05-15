//! `.rpmspec.toml` loading.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use rpm_spec_analyzer::config::Config;

/// Caches loaded configs by the directory they were discovered in. Avoids
/// re-walking and re-parsing `.rpmspec.toml` for every file when many specs
/// share a config.
pub struct ConfigCache {
    explicit: Option<PathBuf>,
    explicit_cache: Option<Arc<Config>>,
    by_dir: HashMap<PathBuf, Arc<Config>>,
    default: Arc<Config>,
}

impl ConfigCache {
    pub fn new(explicit: Option<PathBuf>) -> Self {
        Self {
            explicit,
            explicit_cache: None,
            by_dir: HashMap::new(),
            default: Arc::new(Config::default()),
        }
    }

    /// Resolve the config for `source_path`. The same explicit `--config` is
    /// reused for every call; otherwise the nearest `.rpmspec.toml` walking
    /// upward from the source is looked up once per starting directory.
    pub fn load_for(&mut self, source_path: &Path) -> Result<Arc<Config>> {
        if let Some(path) = self.explicit.clone() {
            if let Some(cached) = &self.explicit_cache {
                return Ok(Arc::clone(cached));
            }
            let cfg = Arc::new(load_from(&path)?);
            self.explicit_cache = Some(Arc::clone(&cfg));
            return Ok(cfg);
        }

        let start_dir = start_dir_for(source_path);
        if let Some(cached) = self.by_dir.get(&start_dir) {
            return Ok(Arc::clone(cached));
        }
        let cfg = match discover_upward(&start_dir) {
            Some(found) => Arc::new(load_from(&found)?),
            None => Arc::clone(&self.default),
        };
        self.by_dir.insert(start_dir, Arc::clone(&cfg));
        Ok(cfg)
    }
}

fn start_dir_for(source_path: &Path) -> PathBuf {
    if source_path.is_file() {
        source_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    } else if source_path.as_os_str() == "-" {
        PathBuf::from(".")
    } else {
        source_path.to_path_buf()
    }
}

fn load_from(path: &Path) -> Result<Config> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read config {}", path.display()))?;
    Config::from_toml_str(&text)
        .with_context(|| format!("failed to parse config {}", path.display()))
}

fn discover_upward(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
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
