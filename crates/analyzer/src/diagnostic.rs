//! Diagnostic types produced by lint rules.
//!
//! `Severity` here is the *configured* level (`Allow`/`Warn`/`Deny`). Rules
//! whose configured severity is `Allow` are never run, so a `Diagnostic`
//! observed by a consumer always carries `Warn` or `Deny`.

use rpm_spec::ast::Span;
use serde::{Deserialize, Serialize};

/// Configured lint level. Maps onto rustc/clippy conventions: `allow` silences
/// the rule, `warn` reports it without affecting exit status, `deny` reports
/// it and fails the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Allow,
    Warn,
    Deny,
}

/// Coarse-grained classification used for filtering and reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LintCategory {
    /// Visual / stylistic conventions.
    Style,
    /// Likely defects: missing fields, redefinitions, contradictions.
    Correctness,
    /// Packaging conventions: changelog, sections, dependencies.
    Packaging,
    /// Build / install / runtime cost.
    Performance,
}

/// How safe an automatic fix is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Applicability {
    /// Safe to apply unattended. Reserved for fixes that preserve meaning.
    MachineApplicable,
    /// Likely correct but may need a human eye. Applied only under
    /// `--fix-suggested`.
    MaybeIncorrect,
    /// Fix is informational; never applied automatically.
    Manual,
}

/// One byte-range replacement in the source string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edit {
    pub span: Span,
    pub replacement: String,
}

/// A coherent set of edits offered together with a [`Diagnostic`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suggestion {
    pub message: String,
    pub edits: Vec<Edit>,
    pub applicability: Applicability,
}

/// Auxiliary span annotation rendered next to the primary location.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

/// A finding emitted by a lint rule.
///
/// `lint_id` is the stable identifier (`"RPM001"`); `lint_name` is the
/// human-readable kebab-case name used in configuration (`"missing-changelog"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub lint_id: &'static str,
    pub lint_name: &'static str,
    pub severity: Severity,
    pub message: String,
    pub primary_span: Span,
    pub labels: Vec<Label>,
    pub suggestions: Vec<Suggestion>,
}

impl Diagnostic {
    pub fn new(
        meta: &'static crate::lint::LintMetadata,
        severity: Severity,
        message: impl Into<String>,
        primary_span: Span,
    ) -> Self {
        Self {
            lint_id: meta.id,
            lint_name: meta.name,
            severity,
            message: message.into(),
            primary_span,
            labels: Vec::new(),
            suggestions: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_label(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label { span, message: message.into() });
        self
    }

    #[must_use]
    pub fn with_suggestion(mut self, suggestion: Suggestion) -> Self {
        self.suggestions.push(suggestion);
        self
    }
}
