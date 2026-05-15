//! Lint session: parse a source, run configured lints, return diagnostics.

use rpm_spec::ast::{Span, SpecFile};
use rpm_spec::parse_result::{Diagnostic as RawParserDiagnostic, ParseResult, Severity as RawParserSeverity};
use rpm_spec::parser::parse_str_with_spans;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::diagnostic::{Diagnostic, Severity};
use crate::lint::Lint;
use crate::registry;

/// Parser-emitted diagnostic, decoupled from the upstream `rpm-spec`
/// types so analyzer consumers do not pick up its semver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ParserDiagnostic {
    pub severity: ParserSeverity,
    /// Stable identifier such as `"rpmspec/E0001"` when the parser tags it.
    pub code: Option<String>,
    pub span: Option<Span>,
    pub message: String,
    pub notes: Vec<String>,
}

/// Severity reported by the parser. Smaller than analyzer [`Severity`] —
/// the parser only emits warnings and errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ParserSeverity {
    Warning,
    Error,
}

impl From<RawParserDiagnostic> for ParserDiagnostic {
    fn from(d: RawParserDiagnostic) -> Self {
        let severity = match d.severity {
            RawParserSeverity::Warning => ParserSeverity::Warning,
            RawParserSeverity::Error => ParserSeverity::Error,
            // Upstream `Severity` is `#[non_exhaustive]`. If a stricter
            // variant (e.g. `Fatal`) is added, default to `Error` so we
            // never silently downgrade severity.
            other => {
                tracing::warn!(?other, "unmapped upstream parser severity; treating as Error");
                ParserSeverity::Error
            }
        };
        Self {
            severity,
            code: d.code,
            span: d.span,
            message: d.message,
            notes: d.notes,
        }
    }
}

/// Result of parsing a source into a `SpecFile` plus parser-level
/// diagnostics. Lint-level findings are produced separately by
/// [`LintSession::run`].
#[derive(Debug)]
#[non_exhaustive]
pub struct ParseOutcome {
    pub spec: SpecFile<Span>,
    pub parser_diagnostics: Vec<ParserDiagnostic>,
}

/// Parse a `.spec` source string with span tracking.
pub fn parse(source: &str) -> ParseOutcome {
    let ParseResult { spec, diagnostics, .. } = parse_str_with_spans(source);
    ParseOutcome {
        spec,
        parser_diagnostics: diagnostics.into_iter().map(ParserDiagnostic::from).collect(),
    }
}

/// Convenience: parse, build a session from `config`, run lints, return both
/// the outcome (for parser diagnostics) and the lint diagnostics. CLI front-
/// ends call this instead of stitching the three steps themselves.
pub fn analyze(source: &str, config: &Config) -> (ParseOutcome, Vec<Diagnostic>) {
    let outcome = parse(source);
    let mut session = LintSession::from_config(config);
    let diags = session.run(&outcome.spec);
    (outcome, diags)
}

/// Owns a configured set of lint rules and runs them sequentially over an AST.
pub struct LintSession {
    lints: Vec<ActiveLint>,
}

struct ActiveLint {
    lint: Box<dyn Lint>,
    /// Severity resolved from `Config` (or the rule's default).
    severity: Severity,
}

impl std::fmt::Debug for LintSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LintSession")
            .field("lints", &self.lints.len())
            .finish()
    }
}

impl LintSession {
    /// Build a session from a parsed `Config`. Rules whose configured
    /// severity is `Allow` are dropped at construction so they never run.
    pub fn from_config(config: &Config) -> Self {
        let mut active: Vec<ActiveLint> = Vec::new();
        for lint in registry::builtin_lints() {
            let meta = lint.metadata();
            let sev = config.severity_for(meta.name, meta.default_severity);
            if sev == Severity::Allow {
                continue;
            }
            active.push(ActiveLint { lint, severity: sev });
        }
        Self { lints: active }
    }

    /// Run every active rule over `spec`. Each rule is invoked in its own
    /// pass; the `Severity` recorded on returned [`Diagnostic`]s matches the
    /// configured level (`Warn` or `Deny`), never `Allow`.
    pub fn run(&mut self, spec: &SpecFile<Span>) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        for ActiveLint { lint, severity } in &mut self.lints {
            lint.visit_spec(spec);
            for mut diag in lint.take_diagnostics() {
                diag.severity = *severity;
                out.push(diag);
            }
        }
        out.sort_by_key(|d| (d.primary_span.start_byte, d.lint_id));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Severity;

    #[test]
    fn analyze_flags_missing_changelog() {
        let cfg = Config::default();
        let (_outcome, diags) = analyze("Name: hello\nVersion: 1\n", &cfg);
        assert!(
            diags.iter().any(|d| d.lint_id == "RPM001"),
            "expected RPM001 in {diags:?}"
        );
    }

    #[test]
    fn analyze_silent_on_clean_spec() {
        let src = "Name: x\n%description\nbody\n%changelog\n\
* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n";
        let cfg = Config::default();
        let (_outcome, diags) = analyze(src, &cfg);
        // Filter to the two lints actually relevant here so the test
        // doesn't become flaky when a new default-warn rule lands.
        let relevant: Vec<_> = diags
            .iter()
            .filter(|d| d.lint_id == "RPM001" || d.lint_id == "RPM002")
            .collect();
        assert!(
            relevant.is_empty(),
            "expected no RPM001/RPM002 diagnostics, got {relevant:?}"
        );
    }

    #[test]
    fn allow_severity_suppresses_lint() {
        let mut cfg = Config::default();
        cfg.apply_cli_overrides::<&str>(&["missing-changelog"], &[], &[]);
        let (_outcome, diags) = analyze("Name: x\n", &cfg);
        assert!(
            !diags.iter().any(|d| d.lint_id == "RPM001"),
            "RPM001 should be silenced by allow override"
        );
    }

    #[test]
    fn deny_overrides_default_warn() {
        let mut cfg = Config::default();
        cfg.apply_cli_overrides::<&str>(&[], &[], &["missing-changelog"]);
        let (_outcome, diags) = analyze("Name: x\n", &cfg);
        let d = diags
            .iter()
            .find(|d| d.lint_id == "RPM001")
            .expect("RPM001 expected");
        assert_eq!(d.severity, Severity::Deny);
    }
}
