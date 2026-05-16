//! `.rpmspec.toml` loading and caching.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use rpm_spec_analyzer::config::Config;

/// Caches loaded configs by the directory they were discovered in. Avoids
/// re-walking and re-parsing `.rpmspec.toml` for every file when many specs
/// share a config.
///
/// Two levels of memoization:
/// 1. `by_dir` — canonicalized starting directory → resolved config.
/// 2. `discover_cache` — canonicalized directory → result of walking up to
///    the nearest `.rpmspec.toml`. Lets a batch of files in sibling
///    directories share the cost of stat'ing common ancestors.
pub struct ConfigCache {
    explicit: Option<PathBuf>,
    explicit_cache: Option<Arc<Config>>,
    /// Directory the explicit config lives in — used as base for
    /// relative paths in the config itself (e.g. `showrc-file`).
    explicit_base_dir: Option<PathBuf>,
    by_dir: HashMap<PathBuf, (Arc<Config>, PathBuf)>,
    discover_cache: HashMap<PathBuf, Option<PathBuf>>,
    default: Arc<Config>,
    /// Base directory associated with the default (no-config) case.
    /// Set once on first use of [`Self::load_for`].
    default_base_dir: Option<PathBuf>,
}

impl ConfigCache {
    /// Build a fresh cache.
    ///
    /// `Some(path)` forces the given config for every lookup
    /// (`--config` mode). `None` triggers upward `.rpmspec.toml`
    /// discovery from each source's directory, falling back to
    /// [`Config::default`] if nothing is found.
    pub fn new(explicit: Option<PathBuf>) -> Self {
        Self {
            explicit,
            explicit_cache: None,
            explicit_base_dir: None,
            by_dir: HashMap::new(),
            discover_cache: HashMap::new(),
            default: Arc::new(Config::default()),
            default_base_dir: None,
        }
    }

    /// Resolve the config for `source_path`, reporting failures on stderr
    /// and flipping `any_io_error` to `true` instead of bubbling the error
    /// up. Used by CLI subcommands that want fail-soft batch behaviour
    /// (one bad file shouldn't abort processing of the rest).
    pub fn load_or_report(
        &mut self,
        source_path: &Path,
        any_io_error: &mut bool,
    ) -> Option<Arc<Config>> {
        match self.load_for(source_path) {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("error: {e:#}");
                *any_io_error = true;
                None
            }
        }
    }

    /// Like [`Self::load_or_report`] but also returns the directory the
    /// config lives in (or the discovery starting directory if no
    /// config file was found). Used by commands that need to resolve
    /// paths declared **inside** the config (e.g. `profile.showrc-file`).
    pub fn load_with_base_dir_or_report(
        &mut self,
        source_path: &Path,
        any_io_error: &mut bool,
    ) -> Option<(Arc<Config>, PathBuf)> {
        match self.load_for_with_base_dir(source_path) {
            Ok(pair) => Some(pair),
            Err(e) => {
                eprintln!("error: {e:#}");
                *any_io_error = true;
                None
            }
        }
    }

    /// Resolve the config for `source_path`. The same explicit `--config` is
    /// reused for every call; otherwise the nearest `.rpmspec.toml` walking
    /// upward from the source is looked up once per starting directory.
    ///
    /// Note: directory paths are resolved through symlinks
    /// (`fs::canonicalize`) before being used as cache keys, so two
    /// invocations referring to the same target through different links
    /// share results.
    ///
    /// # Errors
    ///
    /// Returns an error if the chosen `.rpmspec.toml` can't be read or
    /// doesn't deserialize.
    pub fn load_for(&mut self, source_path: &Path) -> Result<Arc<Config>> {
        self.load_for_with_base_dir(source_path).map(|(c, _)| c)
    }

    /// Variant of [`Self::load_for`] returning both the config and the
    /// directory it was found in (or the discovery starting directory
    /// when the default config is used). Callers needing to interpret
    /// paths *inside* the config (e.g. `showrc-file = "vendor/..."`)
    /// use this to anchor those paths correctly.
    pub fn load_for_with_base_dir(&mut self, source_path: &Path) -> Result<(Arc<Config>, PathBuf)> {
        if let Some(path) = self.explicit.clone() {
            if let (Some(cached), Some(base)) = (&self.explicit_cache, &self.explicit_base_dir) {
                return Ok((Arc::clone(cached), base.clone()));
            }
            let cfg = Arc::new(load_from(&path)?);
            let base = path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            self.explicit_cache = Some(Arc::clone(&cfg));
            self.explicit_base_dir = Some(base.clone());
            return Ok((cfg, base));
        }

        let start_dir = canonicalize_or_keep(&start_dir_for(source_path));
        if let Some((cfg, base)) = self.by_dir.get(&start_dir) {
            return Ok((Arc::clone(cfg), base.clone()));
        }
        let found = self.discover_with_memo(&start_dir);
        let (cfg, base) = match found {
            Some(ref path) => {
                let cfg = Arc::new(load_from(path)?);
                let base = path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| start_dir.clone());
                (cfg, base)
            }
            None => {
                let base = self
                    .default_base_dir
                    .get_or_insert_with(|| start_dir.clone())
                    .clone();
                (Arc::clone(&self.default), base)
            }
        };
        self.by_dir
            .insert(start_dir, (Arc::clone(&cfg), base.clone()));
        Ok((cfg, base))
    }

    /// Walk upward from `start`, memoizing the answer for every directory
    /// visited. Sibling directories share ancestor lookups.
    fn discover_with_memo(&mut self, start: &Path) -> Option<PathBuf> {
        let mut visited: Vec<PathBuf> = Vec::new();
        let mut dir = start.to_path_buf();
        let answer = loop {
            if let Some(cached) = self.discover_cache.get(&dir) {
                break cached.clone();
            }
            visited.push(dir.clone());
            let candidate = dir.join(".rpmspec.toml");
            if candidate.is_file() {
                break Some(candidate);
            }
            if !dir.pop() {
                break None;
            }
        };
        for v in visited {
            self.discover_cache.insert(v, answer.clone());
        }
        answer
    }
}

/// Resolve `p` through symlinks to an absolute path when possible.
/// Returns the input unchanged if canonicalization fails — typically
/// because the path doesn't exist yet, which is fine: the cache key is
/// still stable for a given run.
fn canonicalize_or_keep(p: &Path) -> PathBuf {
    match fs::canonicalize(p) {
        Ok(c) => c,
        Err(e) => {
            // `NotFound` is expected (stdin pseudo-path "-" hasn't been
            // mapped, file just deleted, etc.) — debug. Anything else
            // (permissions, ELOOP, EIO) hints at a real problem the user
            // probably wants to know about, so we warn.
            if e.kind() == std::io::ErrorKind::NotFound {
                tracing::debug!(
                    path = %p.display(),
                    err = %e,
                    "canonicalize failed (not found); using path as-is"
                );
            } else {
                tracing::warn!(
                    path = %p.display(),
                    err = %e,
                    "canonicalize failed; cache key may be inconsistent across formats"
                );
            }
            p.to_path_buf()
        }
    }
}

/// Pick the directory to start the `.rpmspec.toml` walk from. `-` (stdin)
/// uses the current working directory. Existing files use their parent.
/// Anything else (non-existent path, directory, etc.) is treated as a
/// directory itself — the walker either finds a config above or falls
/// back to defaults.
fn start_dir_for(source_path: &Path) -> PathBuf {
    if source_path.as_os_str() == "-" {
        return PathBuf::from(".");
    }
    match fs::metadata(source_path) {
        Ok(meta) if meta.is_file() => source_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(".")),
        Ok(_) => source_path.to_path_buf(),
        Err(e) => {
            tracing::debug!(
                path = %source_path.display(),
                err = %e,
                "could not stat source; using parent directory for config discovery"
            );
            source_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."))
        }
    }
}

fn load_from(path: &Path) -> Result<Config> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read config {}", path.display()))?;
    Config::from_toml_str(&text)
        .with_context(|| format!("failed to parse config {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_config_is_loaded_once() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("explicit.toml");
        fs::write(
            &cfg_path,
            r#"[lints]
missing-changelog = "deny"
"#,
        )
        .unwrap();

        let mut cache = ConfigCache::new(Some(cfg_path.clone()));
        let a = cache.load_for(Path::new("anything.spec")).unwrap();
        let b = cache.load_for(Path::new("other.spec")).unwrap();
        assert!(Arc::ptr_eq(&a, &b), "explicit config should be reused");
    }

    #[test]
    fn discovery_hits_cache_for_same_directory() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join(".rpmspec.toml"),
            r#"[format]
preamble-align-column = 12
"#,
        )
        .unwrap();
        let s1 = tmp.path().join("a.spec");
        let s2 = tmp.path().join("b.spec");
        fs::write(&s1, "").unwrap();
        fs::write(&s2, "").unwrap();

        let mut cache = ConfigCache::new(None);
        let c1 = cache.load_for(&s1).unwrap();
        let c2 = cache.load_for(&s2).unwrap();
        assert!(Arc::ptr_eq(&c1, &c2), "sibling files should share cache");
        assert_eq!(c1.format.preamble_align_column, 12);
    }

    #[test]
    fn missing_config_returns_default() {
        let tmp = tempfile::tempdir().unwrap();
        let s = tmp.path().join("a.spec");
        fs::write(&s, "").unwrap();
        let mut cache = ConfigCache::new(None);
        let cfg = cache.load_for(&s).unwrap();
        // Compare against the analyzer-side default so this test doesn't
        // need to chase a hardcoded constant when the default changes.
        assert_eq!(
            cfg.format.preamble_align_column,
            rpm_spec_analyzer::config::FormatConfig::default().preamble_align_column
        );
    }
}
