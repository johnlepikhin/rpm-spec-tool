//! Convert `rpm-spec-analyzer` diagnostics into LSP diagnostics.

use lsp_types::{
    CodeDescription, Diagnostic as LspDiagnostic, DiagnosticRelatedInformation, DiagnosticSeverity,
    Location, NumberOrString, Range, Uri,
};
use rpm_spec::ast::Span;
use rpm_spec_analyzer::{Diagnostic as AnalyzerDiagnostic, Severity};

use crate::encoding::{LineIndex, PositionEncoding};

/// Source label put into every emitted diagnostic so clients can
/// distinguish ours from other linters and group by source.
pub const SOURCE: &str = "rpm-spec-analyzer";

/// Base URL for lint documentation. Anchors per-rule via the lint
/// name (e.g. `…#missing-changelog`). The catalogue lives in the
/// workspace at `doc/lints-list.md`.
const LINT_DOC_BASE: &str =
    "https://github.com/johnlepikhin/rpm-spec-tool/blob/main/doc/lints-list.md";

fn doc_url_for(lint_name: &str) -> Option<Uri> {
    // Lint names are kebab-case ASCII identifiers — no percent-encoding
    // needed. Parse failures only happen on a malformed `LINT_DOC_BASE`
    // and would indicate a programming error, not user data.
    format!("{LINT_DOC_BASE}#{lint_name}").parse().ok()
}

/// Convert a single analyzer span into an LSP [`Range`].
pub fn span_to_range(span: &Span, source: &str, index: &LineIndex, enc: PositionEncoding) -> Range {
    index.range(source, span.start_byte, span.end_byte, enc)
}

/// Convert one analyzer diagnostic into an LSP diagnostic.
pub fn to_lsp(
    diag: &AnalyzerDiagnostic,
    uri: &Uri,
    source: &str,
    index: &LineIndex,
    enc: PositionEncoding,
) -> LspDiagnostic {
    let severity = match diag.severity {
        Severity::Deny => DiagnosticSeverity::ERROR,
        Severity::Warn => DiagnosticSeverity::WARNING,
        // `Allow` is filtered out by the session before we ever see
        // it, but map it to HINT defensively rather than crash.
        Severity::Allow => DiagnosticSeverity::HINT,
    };
    let related = if diag.labels.is_empty() {
        None
    } else {
        Some(
            diag.labels
                .iter()
                .map(|label| DiagnosticRelatedInformation {
                    location: Location {
                        uri: uri.clone(),
                        range: span_to_range(&label.span, source, index, enc),
                    },
                    message: label.message.clone(),
                })
                .collect(),
        )
    };
    LspDiagnostic {
        range: span_to_range(&diag.primary_span, source, index, enc),
        severity: Some(severity),
        code: Some(NumberOrString::String(diag.lint_id.to_string())),
        // Anchor the diagnostic to the rule's entry in the lint
        // catalogue so editors can render a clickable "more info"
        // link. Bridged parser lints have IDs like `parse/W0001`
        // that don't map to a doc anchor — skip those.
        code_description: if diag.lint_id.starts_with("RPM") {
            doc_url_for(diag.lint_name).map(|href| CodeDescription { href })
        } else {
            None
        },
        source: Some(SOURCE.to_string()),
        message: diag.message.clone(),
        related_information: related,
        tags: None,
        data: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpm_spec_analyzer::config::Config;
    use rpm_spec_analyzer::{Severity, analyze};

    #[test]
    fn missing_changelog_maps_to_warning() {
        let cfg = Config::default();
        let (_outcome, diags) = analyze("Name: hello\nVersion: 1\n", &cfg);
        let analyzer_diag = diags
            .iter()
            .find(|d| d.lint_id == "RPM001")
            .expect("missing-changelog expected");
        assert_eq!(analyzer_diag.severity, Severity::Warn);

        let uri: Uri = "file:///tmp/test.spec".parse().unwrap();
        let source = "Name: hello\nVersion: 1\n";
        let index = LineIndex::new(source);
        let lsp = to_lsp(analyzer_diag, &uri, source, &index, PositionEncoding::Utf16);
        assert_eq!(lsp.severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(lsp.code, Some(NumberOrString::String("RPM001".to_string())));
        assert_eq!(lsp.source.as_deref(), Some(SOURCE));
        // code_description anchors to the catalogue entry.
        let href = lsp.code_description.expect("code_description").href;
        assert!(
            href.as_str().ends_with("#missing-changelog"),
            "unexpected href: {}",
            href.as_str()
        );
    }
}
