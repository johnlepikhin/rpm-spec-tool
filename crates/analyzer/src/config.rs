//! Lint configuration (the `.rpmspec.toml` schema).
//!
//! Schema is the public contract — extensions allowed, breakage is not.

use std::collections::BTreeMap;

use rpm_spec::printer::PrinterConfig;
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

    /// Resolve the configured severity for a lint by its kebab-case name,
    /// falling back to the rule's default if the user did not override it.
    pub fn severity_for(&self, lint_name: &str, default: Severity) -> Severity {
        self.lints.get(lint_name).copied().unwrap_or(default)
    }

    /// Force the given lints to `severity`, replacing any previous setting.
    pub fn apply_overrides<S: AsRef<str>>(&mut self, lint_names: &[S], severity: Severity) {
        for n in lint_names {
            self.lints.insert(n.as_ref().to_owned(), severity);
        }
    }
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
        let mut cfg = FormatConfig::default();
        cfg.preamble_align_column = 0;
        assert!(cfg.to_printer_config().preamble_value_column.is_none());
    }
}
