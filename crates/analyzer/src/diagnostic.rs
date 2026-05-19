//! Diagnostic types produced by lint rules.
//!
//! `Severity` here is the *configured* level (`Allow`/`Warn`/`Deny`). Rules
//! whose configured severity is `Allow` are never run, so a `Diagnostic`
//! observed by a consumer always carries `Warn` or `Deny`.

use rpm_spec::ast::Span;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Configured lint level. Maps onto rustc/clippy conventions: `allow` silences
/// the rule, `warn` reports it without affecting exit status, `deny` reports
/// it and fails the run.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Allow,
    Warn,
    Deny,
}

impl Severity {
    /// `true` when this severity should make the lint silent —
    /// `LintSession::run` and `bridge_parser_diagnostics` use this to
    /// keep their filter logic identical even if a new "silenced"
    /// severity is added later.
    pub fn is_silenced(self) -> bool {
        matches!(self, Severity::Allow)
    }
}

/// Coarse-grained classification used for filtering and reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
pub struct Edit {
    pub span: Span,
    pub replacement: String,
}

impl Edit {
    /// Build an edit that replaces the source bytes covered by `span` with
    /// `replacement`. `span` must align with UTF-8 codepoint boundaries.
    pub fn new(span: Span, replacement: impl Into<String>) -> Self {
        Self {
            span,
            replacement: replacement.into(),
        }
    }
}

/// A coherent set of edits offered together with a [`Diagnostic`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Suggestion {
    pub message: String,
    pub edits: Vec<Edit>,
    pub applicability: Applicability,
}

impl Suggestion {
    /// Build a suggestion. Argument order mirrors the struct field order
    /// (`message`, `edits`, `applicability`) so call sites read like the
    /// type definition.
    pub fn new(message: impl Into<String>, edits: Vec<Edit>, applicability: Applicability) -> Self {
        Self {
            message: message.into(),
            edits,
            applicability,
        }
    }
}

/// Auxiliary span annotation rendered next to the primary location.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Label {
    pub span: Span,
    pub message: String,
}

/// Optional repository attribution attached to a [`Diagnostic`].
///
/// Set by `RPM-REPO-*` rules so consumers (JSON / SARIF output,
/// `matrix deps explain`, future LSP hovers) can answer "which
/// profile and which repo produced this finding, and against which
/// package version was it resolved?". Spec-only rules leave it
/// `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RepoContext {
    /// Profile name as resolved by `rpm-spec-profile` — e.g.
    /// `"redos-7.3-x86_64"`. Distinguishes per-profile differences
    /// in matrix runs.
    pub profile: String,
    /// Repo id as configured in `[profiles.X.repos.<id>]`. `None`
    /// when the finding does not attribute to any specific repo
    /// (e.g. "no provider in any configured repo").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_id: Option<String>,
    /// The package that satisfied / would have satisfied the
    /// requirement, when one was found. `None` for unsat findings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nevra: Option<String>,
}

impl RepoContext {
    pub fn for_profile(profile: impl Into<String>) -> Self {
        Self {
            profile: profile.into(),
            repo_id: None,
            nevra: None,
        }
    }

    #[must_use]
    pub fn with_repo(mut self, repo_id: impl Into<String>) -> Self {
        self.repo_id = Some(repo_id.into());
        self
    }

    #[must_use]
    pub fn with_nevra(mut self, nevra: impl Into<String>) -> Self {
        self.nevra = Some(nevra.into());
        self
    }
}

/// A finding emitted by a lint rule.
///
/// `lint_id` is the stable identifier (`"RPM001"`); `lint_name` is the
/// human-readable kebab-case name used in configuration (`"missing-changelog"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Diagnostic {
    pub lint_id: &'static str,
    pub lint_name: &'static str,
    pub severity: Severity,
    pub message: String,
    pub primary_span: Span,
    pub labels: Vec<Label>,
    pub suggestions: Vec<Suggestion>,
    /// Repository attribution for `RPM-REPO-*` findings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_context: Option<RepoContext>,
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
            repo_context: None,
        }
    }

    #[must_use]
    pub fn with_repo_context(mut self, context: RepoContext) -> Self {
        self.repo_context = Some(context);
        self
    }

    #[must_use]
    pub fn with_label(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label {
            span,
            message: message.into(),
        });
        self
    }

    #[must_use]
    pub fn with_suggestion(mut self, suggestion: Suggestion) -> Self {
        self.suggestions.push(suggestion);
        self
    }
}
