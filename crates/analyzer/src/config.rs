//! Lint configuration (the `.rpmspec.toml` schema).
//!
//! Schema is the public contract — extensions allowed, breakage is not.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::diagnostic::Severity;

/// Whole-file `.rpmspec.toml` schema.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
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
}
