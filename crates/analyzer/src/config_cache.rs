//! `rpmspec.toml` loading and caching.
//!
//! Resolution rules (no walk-up, single source of truth):
//!
//! 1. **Explicit path** — `--config <path>` on the CLI; the LSP
//!    server's project-specific override. Loaded verbatim.
//! 2. **XDG default** — `$XDG_CONFIG_HOME/rpm-spec-tool/rpmspec.toml`
//!    (falls back to `~/.config/rpm-spec-tool/rpmspec.toml` on systems
//!    where the env var isn't set). Resolved via
//!    [`directories::ProjectDirs`]; respects the standard XDG cascade.
//! 3. **Built-in defaults** — when neither the explicit path nor the
//!    XDG file exists, [`Config::default`] is returned. The tool still
//!    works against built-in profiles.
//!
//! The pre-XDG behaviour walked upward from each spec file looking
//! for `.rpmspec.toml`, which made config discovery position-dependent
//! and caused the same spec to be linted with different rules
//! depending on cwd. XDG-only gives one rule per machine and
//! eliminates the surprise.
//!
//! Both the CLI (batch processing) and the LSP server use this
//! directly. The CLI typically resolves the XDG path itself (so it
//! can read `$RPM_SPEC_TOOL_CONFIG` env-var overrides) and feeds the
//! resolved path into `ConfigCache::new(Some(path))`; the LSP server
//! calls [`default_config_path`] from this crate so the behaviour
//! stays in lock-step.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::Config;

/// Errors raised while reading or parsing the resolved `rpmspec.toml`.
///
/// Wraps the underlying [`std::io::Error`] / [`toml::de::Error`] through
/// `#[source]` so callers can walk the causal chain (e.g. anyhow's `{:#}`
/// formatter, or a manual loop over [`std::error::Error::source`]).
#[derive(Debug, thiserror::Error)]
pub enum ConfigCacheError {
    /// Failed to read the chosen config from disk.
    #[error("failed to read config {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Config was read but didn't deserialize as TOML.
    #[error("failed to parse config {path}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

/// The canonical XDG config file location.
///
/// Returns `$XDG_CONFIG_HOME/rpm-spec-tool/rpmspec.toml`, falling
/// back to `~/.config/rpm-spec-tool/rpmspec.toml` per the XDG Base
/// Directory Specification. `None` is only possible on platforms
/// where the user's home directory can't be determined — extremely
/// rare on Linux, but the tool's `compile_error!` already restricts
/// us to Linux.
///
/// The returned path is NOT checked for existence; callers decide
/// whether a missing file is a hard error or a soft fall-back to
/// defaults.
#[must_use]
pub fn default_config_path() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("io", "rpm-spec-tool", "rpm-spec-tool")?;
    Some(dirs.config_dir().join("rpmspec.toml"))
}

/// Caches a single loaded config (either explicit `--config` or the
/// XDG default). Two-level memoization the pre-XDG version needed
/// (per-directory walk-up memo) is gone — the config is now
/// position-independent so a single `Arc<Config>` covers every spec.
///
/// Construction modes:
/// * `ConfigCache::new(Some(path))` — load `path` once on first use.
/// * `ConfigCache::new(None)` — return [`Config::default`] for every
///   query. Callers that want the XDG default file should resolve it
///   via [`default_config_path`] and check existence themselves
///   before passing `Some(...)`.
pub struct ConfigCache {
    /// Explicit path supplied to `new`. `None` means "always default".
    explicit: Option<PathBuf>,
    /// Memoized config — populated on first successful load.
    cached: Option<Arc<Config>>,
    /// Base directory associated with the loaded config (`path.parent()`).
    /// Used as the anchor for relative `showrc-file` references inside
    /// the config itself. For the default-config case, this is `cwd`.
    base_dir: Option<PathBuf>,
    /// Lazily-populated default config for the no-explicit-path branch.
    default: Arc<Config>,
}

impl std::fmt::Debug for ConfigCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigCache")
            .field("explicit", &self.explicit)
            .field("loaded", &self.cached.is_some())
            .finish()
    }
}

impl ConfigCache {
    /// Build a fresh cache.
    ///
    /// `Some(path)` forces the given file for every lookup
    /// (`--config` mode, or CLI-resolved XDG default). `None` means
    /// "use built-in defaults" — equivalent to the user not having
    /// any config at all.
    pub fn new(explicit: Option<PathBuf>) -> Self {
        Self {
            explicit,
            cached: None,
            base_dir: None,
            default: Arc::new(Config::default()),
        }
    }

    /// Resolve the config for `source_path`. The `source_path`
    /// parameter is kept on the signature for back-compat with the
    /// pre-XDG ConfigCache (which used it for walk-up discovery) but
    /// is now ignored — the same config applies to every spec.
    ///
    /// # Errors
    ///
    /// Returns an error if the explicit config can't be read or
    /// doesn't deserialize. Default-only mode never errors.
    pub fn load_for(&mut self, source_path: &Path) -> Result<Arc<Config>, ConfigCacheError> {
        self.load_for_with_base_dir(source_path).map(|(c, _)| c)
    }

    /// Variant of [`Self::load_for`] returning both the config and the
    /// directory it was loaded from (or cwd for the default case).
    /// Callers needing to interpret paths *inside* the config (e.g.
    /// `showrc-file = "vendor/..."`) use this to anchor those paths.
    ///
    /// # Errors
    ///
    /// Same as [`Self::load_for`].
    pub fn load_for_with_base_dir(
        &mut self,
        _source_path: &Path,
    ) -> Result<(Arc<Config>, PathBuf), ConfigCacheError> {
        if let Some(path) = self.explicit.clone() {
            if let (Some(cached), Some(base)) = (&self.cached, &self.base_dir) {
                return Ok((Arc::clone(cached), base.clone()));
            }
            let cfg = Arc::new(load_from(&path)?);
            let base = path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            self.cached = Some(Arc::clone(&cfg));
            self.base_dir = Some(base.clone());
            return Ok((cfg, base));
        }

        // Default mode: anchor at cwd. cwd may fail to read on
        // detached processes; degrade to "." so the config-internal
        // relative-path resolution still has something to work with.
        let base = self
            .base_dir
            .get_or_insert_with(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
            .clone();
        Ok((Arc::clone(&self.default), base))
    }
}

fn load_from(path: &Path) -> Result<Config, ConfigCacheError> {
    let text = fs::read_to_string(path).map_err(|e| ConfigCacheError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    Config::from_toml_str(&text).map_err(|e| ConfigCacheError::Parse {
        path: path.to_path_buf(),
        source: e,
    })
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
    fn no_explicit_returns_default() {
        let tmp = tempfile::tempdir().unwrap();
        let s = tmp.path().join("a.spec");
        fs::write(&s, "").unwrap();
        let mut cache = ConfigCache::new(None);
        let cfg = cache.load_for(&s).unwrap();
        assert_eq!(
            cfg.format.preamble_align_column,
            crate::config::FormatConfig::default().preamble_align_column
        );
    }

    #[test]
    fn default_path_is_under_rpm_spec_tool() {
        // The resolved path varies with $HOME / $XDG_CONFIG_HOME, but
        // the suffix is stable — anchor on it so a future ProjectDirs
        // version that flips the directory name gets caught.
        let path = default_config_path().expect("home dir resolvable in test env");
        let p = path.to_string_lossy();
        assert!(
            p.ends_with("rpm-spec-tool/rpmspec.toml"),
            "expected XDG-shaped path, got: {p}"
        );
    }

    /// Render an error and the full `#[source]` chain into one string,
    /// the way a CLI caller (anyhow's `{:#}`, manual walk, etc.) is
    /// expected to. Used by the tests below to verify that
    /// [`ConfigCacheError`] preserves the underlying OS / TOML error
    /// through `#[source]`.
    fn render_chain(e: &dyn std::error::Error) -> String {
        let mut out = e.to_string();
        let mut src = e.source();
        while let Some(inner) = src {
            out.push_str(": ");
            out.push_str(&inner.to_string());
            src = inner.source();
        }
        out
    }

    #[test]
    fn cache_io_error_preserves_source_chain() {
        // Force an IO error by pointing `--config` at a path that
        // doesn't exist. The cache must surface both its own context
        // ("failed to read config …") and the OS-level cause through
        // `#[source]`.
        let bogus = PathBuf::from("/nonexistent/does-not-exist.toml");
        let mut cache = ConfigCache::new(Some(bogus.clone()));
        let err = cache
            .load_for(Path::new("anything.spec"))
            .expect_err("loading a missing explicit config must fail");

        assert!(
            matches!(err, ConfigCacheError::Io { .. }),
            "expected Io variant, got: {err:?}"
        );

        let chained = render_chain(&err);
        assert!(
            chained.contains("failed to read config"),
            "missing top-level message in chain: {chained}"
        );
        let top = err.to_string();
        assert!(
            chained.len() > top.len(),
            "chain ({chained:?}) was no longer than the top-level message ({top:?}); \
             #[source] is not being walked"
        );
        let suffix = &chained[top.len()..];
        assert!(
            suffix.starts_with(": "),
            "chained suffix should start with ': ', got: {suffix:?}"
        );
    }

    #[test]
    fn cache_parse_error_preserves_source_chain() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("broken.toml");
        // Invalid TOML: dangling key with no value.
        fs::write(&cfg_path, "this is = = not = toml\n").unwrap();

        let mut cache = ConfigCache::new(Some(cfg_path.clone()));
        let err = cache
            .load_for(Path::new("anything.spec"))
            .expect_err("malformed TOML must fail");

        assert!(
            matches!(err, ConfigCacheError::Parse { .. }),
            "expected Parse variant, got: {err:?}"
        );

        let chained = render_chain(&err);
        assert!(
            chained.contains("failed to parse config"),
            "missing top-level message in chain: {chained}"
        );
        let top = err.to_string();
        assert!(
            chained.len() > top.len(),
            "chain ({chained:?}) was no longer than the top-level message ({top:?}); \
             #[source] is not being walked"
        );
    }
}
