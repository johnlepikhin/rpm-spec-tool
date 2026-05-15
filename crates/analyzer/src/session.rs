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
///
/// Parser-emitted diagnostics (the recoverable issues `rpm-spec` flags
/// while parsing — unrecognized lines, unbalanced rich deps, ...) are
/// bridged into the lint pipeline through [`bridge_parser_diagnostics`],
/// so they obey severity overrides and `--format json/sarif` just like
/// regular lints.
pub fn analyze(source: &str, config: &Config) -> (ParseOutcome, Vec<Diagnostic>) {
    let outcome = parse(source);
    let mut session = LintSession::from_config(config);
    let mut diags = session.run(&outcome.spec, source);
    diags.extend(bridge_parser_diagnostics(&outcome.parser_diagnostics, config));
    sort_and_dedup(&mut diags);
    (outcome, diags)
}

/// Sort diagnostics by source location and drop exact duplicates.
///
/// Safety net against rules that anchor multiple findings at the same
/// span (e.g. one diagnostic per AST node when the same source
/// location is visited from several angles). Keying on the message
/// preserves distinct findings that happen to share a span but
/// suggest different fixes.
fn sort_and_dedup(diags: &mut Vec<Diagnostic>) {
    diags.sort_by(|a, b| {
        a.primary_span
            .start_byte
            .cmp(&b.primary_span.start_byte)
            .then_with(|| a.lint_id.cmp(b.lint_id))
            .then_with(|| a.message.cmp(&b.message))
    });
    diags.dedup_by(|a, b| {
        a.lint_id == b.lint_id && a.primary_span == b.primary_span && a.message == b.message
    });
}

/// A parser code that we expose to users as a lint.
///
/// Codes that are also covered by AST-based rules (e.g. `rpmspec/W0025`
/// → `changelog-implausible-date` is detected separately) are omitted
/// from this table to avoid double-reporting.
struct BridgeEntry {
    parser_code: &'static str,
    metadata: &'static crate::lint::LintMetadata,
}

/// Static metadata for every bridged parser code. Kept in one place so
/// new mappings are a one-line addition.
mod bridged {
    use crate::diagnostic::{LintCategory, Severity};
    use crate::lint::LintMetadata;

    macro_rules! bridged {
        ($vis:vis $name:ident = ($id:literal, $kebab:literal, $desc:literal, $sev:expr)) => {
            $vis static $name: LintMetadata = LintMetadata {
                id: $id,
                name: $kebab,
                description: $desc,
                default_severity: $sev,
                category: LintCategory::Correctness,
            };
        };
    }

    bridged!(pub NO_PROGRESS = (
        "parse/E0001", "parse-no-progress",
        "Parser couldn't consume any input at this position — the spec is unparseable here.",
        Severity::Deny
    ));
    bridged!(pub UNTERMINATED_COND = (
        "parse/E0002", "parse-unterminated-conditional",
        "An `%if`/`%ifarch`/`%ifos` block was opened without a matching `%endif`.",
        Severity::Deny
    ));
    bridged!(pub LINE_NOT_RECOGNIZED = (
        "parse/W0002", "parse-line-not-recognized",
        "Line did not match any known top-level construction.",
        Severity::Warn
    ));
    bridged!(pub STRAY_PERCENT = (
        "parse/W0001", "parse-stray-percent",
        "A `%` appeared in text without forming a valid macro reference — write `%%` for a literal percent.",
        Severity::Warn
    ));
    bridged!(pub UNTERMINATED_MACRO = (
        "parse/W0004", "parse-unterminated-macro",
        "A `%(...)`, `%[...]`, or `%{...}` macro reference was not closed before EOF or terminator.",
        Severity::Warn
    ));
    bridged!(pub MULTIPLE_ELSE = (
        "parse/W0005", "parse-multiple-else",
        "Conditional block has more than one `%else`; only the last one is honoured.",
        Severity::Warn
    ));
    bridged!(pub MALFORMED_CHANGELOG = (
        "parse/W0023", "parse-malformed-changelog-header",
        "Changelog entry header (`* Weekday Month Day Year ...`) could not be parsed.",
        Severity::Warn
    ));
}

/// Static mapping table. `rpmspec/W0025` is deliberately absent — it's
/// surfaced by the dedicated [`crate::rules::changelog_health::ChangelogImplausibleDate`]
/// AST rule instead, to give users a single overridable lint name.
const BRIDGE: &[BridgeEntry] = &[
    BridgeEntry { parser_code: "rpmspec/E0001", metadata: &bridged::NO_PROGRESS },
    BridgeEntry { parser_code: "rpmspec/E0002", metadata: &bridged::UNTERMINATED_COND },
    BridgeEntry { parser_code: "rpmspec/W0001", metadata: &bridged::STRAY_PERCENT },
    BridgeEntry { parser_code: "rpmspec/W0002", metadata: &bridged::LINE_NOT_RECOGNIZED },
    BridgeEntry { parser_code: "rpmspec/W0004", metadata: &bridged::UNTERMINATED_MACRO },
    BridgeEntry { parser_code: "rpmspec/W0005", metadata: &bridged::MULTIPLE_ELSE },
    BridgeEntry { parser_code: "rpmspec/W0023", metadata: &bridged::MALFORMED_CHANGELOG },
];

/// Translate `rpm-spec` parser diagnostics into lint-level [`Diagnostic`]s.
/// Unknown parser codes are dropped — they surface in `ParseOutcome` for
/// callers who want them but don't pollute lint output.
pub(crate) fn bridge_parser_diagnostics(
    parser_diagnostics: &[ParserDiagnostic],
    config: &Config,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for pd in parser_diagnostics {
        let Some(code) = pd.code.as_deref() else { continue };
        let Some(entry) = BRIDGE.iter().find(|e| e.parser_code == code) else { continue };
        let severity = config.severity_for(entry.metadata.name, entry.metadata.default_severity);
        if severity.is_silenced() {
            continue;
        }
        // Parser diagnostics with no span hit the spec root (0..0) —
        // best effort, but we never want a `Diagnostic` without a span.
        let span = pd.span.unwrap_or_default();
        out.push(Diagnostic::new(
            entry.metadata,
            severity,
            pd.message.clone(),
            span,
        ));
    }
    out
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
            if sev.is_silenced() {
                continue;
            }
            active.push(ActiveLint { lint, severity: sev });
        }
        Self { lints: active }
    }

    /// Run every active rule over `spec`. Each rule is invoked in its own
    /// pass; the `Severity` recorded on returned [`Diagnostic`]s matches the
    /// configured level (`Warn` or `Deny`), never `Allow`.
    ///
    /// `source` is the original `.spec` source text, passed to every rule
    /// via [`Lint::set_source`] before the visit pass starts. Rules that
    /// inspect raw bytes (whitespace, tabs, exact line slicing) use it;
    /// AST-only rules ignore it.
    ///
    /// Diagnostics are returned in lint-emission order. Callers that
    /// want sorted, deduplicated output should use [`analyze`] (which
    /// runs [`sort_and_dedup`]) instead of post-processing themselves.
    pub fn run(&mut self, spec: &SpecFile<Span>, source: &str) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        for ActiveLint { lint, severity } in &mut self.lints {
            lint.set_source(source);
            lint.visit_spec(spec);
            for mut diag in lint.take_diagnostics() {
                diag.severity = *severity;
                out.push(diag);
            }
        }
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

    // -----------------------------------------------------------------
    // bridge_parser_diagnostics
    // -----------------------------------------------------------------

    fn pd(code: Option<&str>, span: Option<Span>, msg: &str) -> ParserDiagnostic {
        ParserDiagnostic {
            severity: ParserSeverity::Warning,
            code: code.map(str::to_owned),
            span,
            message: msg.into(),
            notes: Vec::new(),
        }
    }

    #[test]
    fn sort_and_dedup_collapses_identical_diagnostics() {
        use crate::diagnostic::{LintCategory, Severity};
        use crate::lint::LintMetadata;

        static META: LintMetadata = LintMetadata {
            id: "RPM999",
            name: "test-lint",
            description: "",
            default_severity: Severity::Warn,
            category: LintCategory::Correctness,
        };
        let anchor = Span::from_bytes(10, 20);
        let mk = |msg: &str| Diagnostic::new(&META, Severity::Warn, msg.to_owned(), anchor);
        // Three diagnostics: two identical (same span + message), one
        // with a different message at the same anchor.
        let mut diags = vec![mk("same"), mk("same"), mk("different")];
        sort_and_dedup(&mut diags);
        assert_eq!(diags.len(), 2, "expected exact-dup to collapse: {diags:?}");
        // Distinct messages must both survive.
        let messages: Vec<_> = diags.iter().map(|d| d.message.as_str()).collect();
        assert!(messages.contains(&"same"));
        assert!(messages.contains(&"different"));
    }

    #[test]
    fn bridge_drops_diag_without_code() {
        let diags = bridge_parser_diagnostics(&[pd(None, None, "x")], &Config::default());
        assert!(diags.is_empty());
    }

    #[test]
    fn bridge_drops_unknown_code() {
        let diags = bridge_parser_diagnostics(
            &[pd(Some("rpmspec/Z9999"), None, "unknown")],
            &Config::default(),
        );
        assert!(diags.is_empty());
    }

    #[test]
    fn bridge_silenced_by_allow_override() {
        let mut cfg = Config::default();
        cfg.apply_cli_overrides::<&str>(&["parse-line-not-recognized"], &[], &[]);
        let diags = bridge_parser_diagnostics(
            &[pd(Some("rpmspec/W0002"), None, "msg")],
            &cfg,
        );
        assert!(diags.is_empty(), "allow override must suppress bridged diag");
    }

    #[test]
    fn bridge_uses_span_default_when_missing() {
        let diags = bridge_parser_diagnostics(
            &[pd(Some("rpmspec/W0002"), None, "msg")],
            &Config::default(),
        );
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].primary_span, Span::default());
    }

    #[test]
    fn bridge_propagates_message_and_code() {
        let span = Span::from_bytes(10, 20);
        let diags = bridge_parser_diagnostics(
            &[pd(Some("rpmspec/W0001"), Some(span), "stray % here")],
            &Config::default(),
        );
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "parse/W0001");
        assert_eq!(diags[0].lint_name, "parse-stray-percent");
        assert!(diags[0].message.contains("stray % here"));
        assert_eq!(diags[0].primary_span, span);
    }

    #[test]
    fn bridge_covers_every_table_entry() {
        // Table-driven regression: every BRIDGE row must round-trip
        // (parser_code -> lint_id) so adding a new entry can't silently
        // break the mapping.
        for entry in BRIDGE {
            let diags = bridge_parser_diagnostics(
                &[pd(Some(entry.parser_code), None, "x")],
                &Config::default(),
            );
            assert_eq!(diags.len(), 1, "entry {} produced no diag", entry.parser_code);
            assert_eq!(diags[0].lint_id, entry.metadata.id);
            assert_eq!(diags[0].lint_name, entry.metadata.name);
        }
    }
}
