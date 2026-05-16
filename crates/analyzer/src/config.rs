//! Lint configuration (the `.rpmspec.toml` schema).
//!
//! Schema is the public contract — extensions allowed, breakage is not.

use std::collections::BTreeMap;

use rpm_spec::printer::PrinterConfig;
use rpm_spec_profile::ProfileEntry;
use serde::{Deserialize, Serialize};

use crate::diagnostic::Severity;

/// Whole-file `.rpmspec.toml` schema.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct Config {
    #[serde(default)]
    pub lints: BTreeMap<String, Severity>,
    #[serde(default)]
    pub format: FormatConfig,
    #[serde(default)]
    pub shellcheck: ShellcheckConfig,
    /// Active distribution profile (built-in name or a key from
    /// [`Self::profiles`]). When unset, the resolver falls back to the
    /// `generic` built-in. Override-able via the `--profile` CLI flag.
    #[serde(default)]
    pub profile: Option<String>,
    /// User-defined named profiles. See `doc/profiles.md` for the
    /// semantics of layering (`extends` + `showrc-file` + overrides).
    #[serde(default)]
    pub profiles: BTreeMap<String, ProfileEntry>,
    /// "Warnings-as-errors" toggle — when `true`, any rule that
    /// resolves to [`Severity::Warn`] is promoted to [`Severity::Deny`]
    /// at runtime. Triggered from the CLI by `--deny warnings`
    /// (clippy convention); not exposed in TOML to keep the schema
    /// stable. Rules explicitly demoted to `Allow` keep that level.
    #[serde(skip)]
    pub warnings_as_errors: bool,
}

/// Configuration for the `shellcheck` umbrella lint (RPM200).
///
/// Severity is controlled through `[lints]` like every other rule
/// (`shellcheck = "warn"`); this struct only carries options that have
/// no natural representation as a severity (binary path, per-SC-code
/// disable list).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default, rename_all = "kebab-case")]
#[non_exhaustive]
pub struct ShellcheckConfig {
    /// Override path to the shellcheck binary. When `None`, the rule
    /// looks up `shellcheck` in `$PATH`.
    pub binary: Option<String>,
    /// SC codes to suppress *in addition to* the built-in RPM-context
    /// baseline. Accepts the canonical `SC<n>` form (case-insensitive)
    /// or a bare number such as `"2086"`.
    pub disable: Vec<String>,
    /// SC codes to re-enable from the built-in baseline. Same accepted
    /// forms as `disable`. Useful for users who explicitly want
    /// `SC2164` (`pushd … || exit`) etc.
    pub enable: Vec<String>,
    /// Shell dialect passed to `shellcheck --shell=<dialect>`. Defaults
    /// to `bash`, which matches `/bin/sh` on every major RPM-based
    /// distribution. Set to `sh` for strict POSIX checking. Accepted
    /// values: `sh`, `bash`, `dash`, `ksh`.
    pub shell: Option<String>,
    /// Per-section timeout, in seconds, for the shellcheck subprocess.
    /// On timeout the process is killed and a single `RPM201` is
    /// emitted; subsequent sections are skipped. Defaults to 30s.
    pub timeout_secs: Option<u64>,
}

/// Subset that affects the pretty-printer. Mapped onto
/// [`rpm_spec::printer::PrinterConfig`] at the boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default, rename_all = "kebab-case")]
#[non_exhaustive]
pub struct FormatConfig {
    /// Column at which preamble values are aligned. `0` means a single space.
    pub preamble_align_column: u32,
    /// Spaces per nesting level inside `%if` blocks.
    pub conditional_indent: u32,
}

impl Default for FormatConfig {
    fn default() -> Self {
        Self {
            preamble_align_column: 16,
            conditional_indent: 0,
        }
    }
}

impl FormatConfig {
    /// Build a [`PrinterConfig`] reflecting this configuration. `column = 0`
    /// is the documented sentinel for "single-space separator".
    pub fn to_printer_config(&self) -> PrinterConfig {
        let preamble_column = if self.preamble_align_column == 0 {
            None
        } else {
            Some(self.preamble_align_column as usize)
        };
        PrinterConfig::new()
            .with_indent(self.conditional_indent as usize)
            .with_preamble_value_column(preamble_column)
    }
}

impl Config {
    /// Parse a `.rpmspec.toml` source string.
    pub fn from_toml_str(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// Resolve the active profile against this config.
    ///
    /// `base_dir` is the directory `.rpmspec.toml` lives in (used to
    /// resolve relative `showrc-file` paths). `opts` carries CLI-time
    /// inputs — `--profile` override and any `--define NAME VALUE`
    /// arguments. Constructors:
    /// * `ResolveOptions::default()` — no CLI overrides at all (used
    ///   by `profile list`, tests).
    /// * `ResolveOptions::with_override(Some("rhel-9"))` — only
    ///   `--profile`, no defines.
    /// * Struct literal — when both `--profile` and `--define` apply.
    pub fn resolve_profile(
        &self,
        base_dir: &std::path::Path,
        opts: rpm_spec_profile::ResolveOptions<'_>,
    ) -> Result<rpm_spec_profile::Profile, rpm_spec_profile::ResolveError> {
        let section =
            rpm_spec_profile::ProfileSection::new(self.profile.clone(), self.profiles.clone());
        rpm_spec_profile::resolve_profile(&section, base_dir, opts)
    }

    /// Resolve the configured severity for a lint by its kebab-case name,
    /// falling back to the rule's default if the user did not override it.
    ///
    /// Honours [`Self::warnings_as_errors`]: when set, *any* resolved
    /// `Warn` is promoted to `Deny`. This includes
    ///
    /// 1. the rule's default-severity, when no per-lint override is set,
    /// 2. an explicit `--warn LINT` override (matches clippy's
    ///    `-W foo -D warnings` semantics — `-W` declares the level the
    ///    rule starts at, `-D warnings` then promotes everything still
    ///    at Warn), and
    /// 3. a TOML `lints.LINT = "warn"` entry.
    ///
    /// Pinning a specific lint at Warn under `-D warnings` therefore
    /// requires `--allow LINT` (an explicit `Allow` override is *not*
    /// promoted — the user clearly meant "suppress").
    pub fn severity_for(&self, lint_name: &str, default: Severity) -> Severity {
        let resolved = self.lints.get(lint_name).copied().unwrap_or(default);
        if self.warnings_as_errors && resolved == Severity::Warn {
            Severity::Deny
        } else {
            resolved
        }
    }

    /// Force the given lints to `severity`, replacing any previous setting.
    pub fn apply_overrides<S: AsRef<str>>(&mut self, lint_names: &[S], severity: Severity) {
        for n in lint_names {
            self.lints.insert(n.as_ref().to_owned(), severity);
        }
    }

    /// Apply CLI severity overrides in the conventional clippy-style
    /// order: `allow` first, then `warn`, then `deny`.
    ///
    /// Resolution rules:
    /// * **Across lists:** later groups override earlier ones, so a lint
    ///   present in both `--allow` and `--deny` ends up at `Deny`.
    /// * **Within one list:** duplicates resolve to last-write-wins
    ///   (e.g. `--deny foo --deny foo` is no different from one flag).
    /// * **`warnings` is a meta-name** in any list: `--deny warnings`
    ///   sets [`Self::warnings_as_errors`], `--allow warnings` clears
    ///   it, `--warn warnings` is a no-op (the default). The literal
    ///   string is not registered as a lint name.
    pub fn apply_cli_overrides<S: AsRef<str>>(&mut self, allow: &[S], warn: &[S], deny: &[S]) {
        // Split meta-name `warnings` out of each list before applying
        // per-lint overrides. Order matters: allow first, warn (no-op
        // on the meta), deny last — so `--deny warnings --allow warnings`
        // ends up with `warnings_as_errors=false` (last-write-wins).
        let (allow_lints, allow_meta) = split_warnings_meta(allow);
        let (warn_lints, warn_meta) = split_warnings_meta(warn);
        let (deny_lints, deny_meta) = split_warnings_meta(deny);

        self.apply_overrides(&allow_lints, Severity::Allow);
        self.apply_overrides(&warn_lints, Severity::Warn);
        self.apply_overrides(&deny_lints, Severity::Deny);

        // Meta-name resolution mirrors the same allow→warn→deny order.
        if allow_meta {
            self.warnings_as_errors = false;
        }
        if warn_meta {
            // No-op: this is the baseline. Kept as an explicit branch
            // so a future contributor sees the intent rather than
            // discovering it's missing.
        }
        if deny_meta {
            self.warnings_as_errors = true;
        }
    }
}

/// Recognised meta-name for the "warnings-as-errors" toggle. Borrowed
/// from clippy's `--deny warnings` / `--allow warnings`.
const META_WARNINGS: &str = "warnings";

/// Pull [`META_WARNINGS`] entries out of `list`, returning the
/// remaining lint names and a boolean signalling that the meta was
/// present.
fn split_warnings_meta<S: AsRef<str>>(list: &[S]) -> (Vec<String>, bool) {
    let mut meta = false;
    let mut lints = Vec::with_capacity(list.len());
    for item in list {
        if item.as_ref() == META_WARNINGS {
            meta = true;
        } else {
            lints.push(item.as_ref().to_owned());
        }
    }
    (lints, meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_toml_round_trip() {
        let toml_str = r#"
[lints]
missing-changelog = "deny"

[format]
preamble-align-column = 20
"#;
        let cfg = Config::from_toml_str(toml_str).unwrap();
        assert_eq!(
            cfg.severity_for("missing-changelog", Severity::Warn),
            Severity::Deny
        );
        assert_eq!(cfg.format.preamble_align_column, 20);
    }

    #[test]
    fn unknown_field_rejected() {
        let toml_str = "unknown-key = 1\n";
        assert!(Config::from_toml_str(toml_str).is_err());
    }

    #[test]
    fn apply_overrides_replaces_severity() {
        let mut cfg = Config::default();
        cfg.lints.insert("foo".into(), Severity::Warn);
        cfg.apply_overrides(&["foo", "bar"], Severity::Deny);
        assert_eq!(cfg.severity_for("foo", Severity::Allow), Severity::Deny);
        assert_eq!(cfg.severity_for("bar", Severity::Allow), Severity::Deny);
    }

    #[test]
    fn to_printer_config_zero_means_single_space() {
        let cfg = FormatConfig {
            preamble_align_column: 0,
            ..FormatConfig::default()
        };
        assert!(cfg.to_printer_config().preamble_value_column.is_none());
    }

    #[test]
    fn cli_overrides_priority_deny_over_allow() {
        let mut cfg = Config::default();
        // Same lint listed in both `allow` and `deny`: deny applies last
        // and must win.
        cfg.apply_cli_overrides::<&str>(&["foo"], &[], &["foo"]);
        assert_eq!(cfg.severity_for("foo", Severity::Warn), Severity::Deny);
    }

    #[test]
    fn cli_overrides_priority_warn_over_allow() {
        let mut cfg = Config::default();
        cfg.apply_cli_overrides::<&str>(&["bar"], &["bar"], &[]);
        assert_eq!(cfg.severity_for("bar", Severity::Deny), Severity::Warn);
    }

    // ----- `-D warnings` (clippy-style meta) -----

    #[test]
    fn deny_warnings_meta_promotes_warn_to_deny() {
        let mut cfg = Config::default();
        cfg.apply_cli_overrides::<&str>(&[], &[], &["warnings"]);
        // Default-Warn rule becomes Deny under the meta.
        assert_eq!(
            cfg.severity_for("missing-changelog", Severity::Warn),
            Severity::Deny
        );
        // Default-Allow stays Allow (silenced is silenced).
        assert_eq!(
            cfg.severity_for("opt-in-rule", Severity::Allow),
            Severity::Allow
        );
        // Default-Deny stays Deny.
        assert_eq!(cfg.severity_for("must-fix", Severity::Deny), Severity::Deny);
        // No `"warnings"` entry leaked into the lint table.
        assert!(!cfg.lints.contains_key("warnings"));
    }

    #[test]
    fn deny_warnings_respects_explicit_allow_per_lint() {
        let mut cfg = Config::default();
        // `--allow foo --deny warnings` — foo stays silenced.
        cfg.apply_cli_overrides::<&str>(&["foo"], &[], &["warnings"]);
        assert_eq!(cfg.severity_for("foo", Severity::Warn), Severity::Allow);
        // Other rules promote normally.
        assert_eq!(cfg.severity_for("bar", Severity::Warn), Severity::Deny);
    }

    #[test]
    fn allow_warnings_meta_clears_the_promotion() {
        let mut cfg = Config::default();
        cfg.apply_cli_overrides::<&str>(&[], &[], &["warnings"]);
        cfg.apply_cli_overrides::<&str>(&["warnings"], &[], &[]);
        assert!(!cfg.warnings_as_errors);
        assert_eq!(
            cfg.severity_for("missing-changelog", Severity::Warn),
            Severity::Warn
        );
    }

    #[test]
    fn warn_warnings_meta_is_a_no_op() {
        // `--warn warnings` is the baseline state and intentionally
        // a no-op — the explicit branch in `apply_cli_overrides`
        // exists to document the intent rather than affect anything.
        // This test pins that contract so a future refactor that
        // accidentally makes it do something fails loudly.
        let mut cfg = Config::default();
        cfg.apply_cli_overrides::<&str>(&[], &["warnings"], &[]);
        assert!(!cfg.warnings_as_errors);
        assert!(
            cfg.lints.is_empty(),
            "meta-name must not leak as a lint key"
        );
    }

    #[test]
    fn shellcheck_config_round_trip() {
        let toml_str = r#"
[shellcheck]
binary = "/opt/sc"
disable = ["SC2086", "2155"]
"#;
        let cfg = Config::from_toml_str(toml_str).unwrap();
        assert_eq!(cfg.shellcheck.binary.as_deref(), Some("/opt/sc"));
        assert_eq!(cfg.shellcheck.disable, vec!["SC2086", "2155"]);
    }
}
