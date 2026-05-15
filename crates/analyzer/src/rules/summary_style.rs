//! Phase 4 lints over the `Summary:` preamble tag.
//!
//! Four rules share `visit_preamble` + a single helper that extracts
//! the literal text of a `Summary:` value (skipping macro-only or
//! macro-mixed lines we can't safely reason about):
//!
//! - **RPM055 `summary-ends-with-dot`** — `Summary: Foo.` reads like a
//!   sentence and gets joined awkwardly with the description in package
//!   manager UIs. Strip the trailing `.`.
//! - **RPM056 `summary-not-capitalized`** — same reasoning: lowercase
//!   first letter looks wrong in listings.
//! - **RPM057 `summary-too-long`** — Summary lines wider than ~80 chars
//!   get truncated in `dnf list` / GUI cards. Hardcoded 80 today; will
//!   move into the per-lint config system together with profiles.
//! - **RPM058 `name-in-summary`** — repeating the package name inside
//!   its own Summary is redundant and steals characters from the
//!   actual description.

use rpm_spec::ast::{PreambleItem, Span, SpecFile, Tag, TagValue, TextSegment};

use crate::diagnostic::{Applicability, Diagnostic, Edit, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::package_name;
use crate::visit::Visit;

// =====================================================================
// Shared metadata
// =====================================================================

/// Lint metadata for RPM055 `summary-ends-with-dot`.
pub static ENDS_WITH_DOT_METADATA: LintMetadata = LintMetadata {
    id: "RPM055",
    name: "summary-ends-with-dot",
    description: "Summary should not end with a period.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Lint metadata for RPM056 `summary-not-capitalized`.
pub static NOT_CAPITALIZED_METADATA: LintMetadata = LintMetadata {
    id: "RPM056",
    name: "summary-not-capitalized",
    description: "Summary should start with an uppercase letter.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Lint metadata for RPM057 `summary-too-long`.
pub static TOO_LONG_METADATA: LintMetadata = LintMetadata {
    id: "RPM057",
    name: "summary-too-long",
    description: "Summary is longer than the recommended maximum (80 chars).",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Lint metadata for RPM058 `name-in-summary`.
pub static NAME_IN_SUMMARY_METADATA: LintMetadata = LintMetadata {
    id: "RPM058",
    name: "name-in-summary",
    description: "Package name should not appear in its own Summary.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

// TODO(profile-system): replace the hardcoded 80 with a per-lint
// config knob such as `[lints.summary-too-long] max = N` once the
// profile/per-lint-options machinery lands.
const MAX_SUMMARY_LEN: usize = 80;

// =====================================================================
// Helpers
// =====================================================================

/// Pull the literal text and span of a `Summary:` preamble item, but
/// only when the value is a single literal segment we can safely
/// inspect. Returns `None` when the Summary contains macros or has any
/// other shape that would make textual reasoning unsafe.
fn summary_literal(item: &PreambleItem<Span>) -> Option<(&str, Span)> {
    if !matches!(item.tag, Tag::Summary) {
        return None;
    }
    let TagValue::Text(text) = &item.value else {
        return None;
    };
    let s = text.literal_str()?;
    Some((s, item.data))
}

/// Byte offset within `item.data` where the `Summary:` value starts —
/// used so auto-fixes touch only the value bytes, not the tag header.
/// We locate the first colon and skip following whitespace; the parser
/// keeps the original bytes in the span so this scan is cheap.
fn value_start_offset(line: &str) -> Option<usize> {
    let colon = line.find(':')?;
    let mut idx = colon + 1;
    let bytes = line.as_bytes();
    while idx < bytes.len() && (bytes[idx] == b' ' || bytes[idx] == b'\t') {
        idx += 1;
    }
    Some(idx)
}

// =====================================================================
// RPM055 summary-ends-with-dot
// =====================================================================

#[derive(Debug, Default)]
pub struct SummaryEndsWithDot {
    diagnostics: Vec<Diagnostic>,
    source: Option<String>,
}

impl SummaryEndsWithDot {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for SummaryEndsWithDot {
    fn visit_preamble(&mut self, node: &'ast PreambleItem<Span>) {
        let Some((value, span)) = summary_literal(node) else { return };
        let trimmed = value.trim_end();
        if !trimmed.ends_with('.') {
            return;
        }
        let mut diag = Diagnostic::new(
            &ENDS_WITH_DOT_METADATA,
            Severity::Warn,
            "Summary ends with a `.`",
            span,
        );

        // Locate the trailing dot inside the source line so the auto-fix
        // touches just the period.
        if let Some(source) = &self.source {
            let line = &source
                [span.start_byte.min(source.len())..span.end_byte.min(source.len())];
            if let Some(val_start) = value_start_offset(line) {
                let line_value = line[val_start..].trim_end();
                if let Some(dot_byte_in_value) = line_value.rfind('.') {
                    let abs_start =
                        span.start_byte + val_start + dot_byte_in_value;
                    let edit_span = Span::from_bytes(abs_start, abs_start + 1);
                    diag = diag.with_suggestion(Suggestion::new(
                        "remove the trailing period",
                        vec![Edit::new(edit_span, "")],
                        Applicability::MachineApplicable,
                    ));
                }
            }
        }
        let _ = value; // silence unused warning when source is None
        let _ = trimmed;
        self.diagnostics.push(diag);
    }
}

impl Lint for SummaryEndsWithDot {
    fn metadata(&self) -> &'static LintMetadata {
        &ENDS_WITH_DOT_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_source(&mut self, source: &str) {
        self.source = Some(source.to_owned());
    }
}

// =====================================================================
// RPM056 summary-not-capitalized
// =====================================================================

#[derive(Debug, Default)]
pub struct SummaryNotCapitalized {
    diagnostics: Vec<Diagnostic>,
    source: Option<String>,
}

impl SummaryNotCapitalized {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for SummaryNotCapitalized {
    fn visit_preamble(&mut self, node: &'ast PreambleItem<Span>) {
        let Some((value, span)) = summary_literal(node) else { return };
        let value = value.trim_start();
        let Some(first) = value.chars().next() else { return };
        if !first.is_alphabetic() || !first.is_lowercase() {
            return;
        }
        let mut diag = Diagnostic::new(
            &NOT_CAPITALIZED_METADATA,
            Severity::Warn,
            format!("Summary should start with an uppercase letter (`{first}`)"),
            span,
        );

        if let Some(source) = &self.source {
            let line = &source
                [span.start_byte.min(source.len())..span.end_byte.min(source.len())];
            if let Some(val_start) = value_start_offset(line) {
                // Skip leading whitespace inside the value, then find the
                // first character byte to replace.
                let trimmed = &line[val_start..];
                let ws_skip = trimmed.len() - trimmed.trim_start().len();
                let abs_first = span.start_byte + val_start + ws_skip;
                // `first` is the first char of `value` (== trim_start
                // result), so its byte length tells us the edit width.
                let abs_end = abs_first + first.len_utf8();
                let replacement: String = first.to_uppercase().collect();
                diag = diag.with_suggestion(Suggestion::new(
                    "capitalize the first letter",
                    vec![Edit::new(
                        Span::from_bytes(abs_first, abs_end),
                        replacement,
                    )],
                    Applicability::MachineApplicable,
                ));
            }
        }
        self.diagnostics.push(diag);
    }
}

impl Lint for SummaryNotCapitalized {
    fn metadata(&self) -> &'static LintMetadata {
        &NOT_CAPITALIZED_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_source(&mut self, source: &str) {
        self.source = Some(source.to_owned());
    }
}

// =====================================================================
// RPM057 summary-too-long
// =====================================================================

#[derive(Debug, Default)]
pub struct SummaryTooLong {
    diagnostics: Vec<Diagnostic>,
}

impl SummaryTooLong {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for SummaryTooLong {
    fn visit_preamble(&mut self, node: &'ast PreambleItem<Span>) {
        let Some((value, span)) = summary_literal(node) else { return };
        let len = value.trim().chars().count();
        if len > MAX_SUMMARY_LEN {
            self.diagnostics.push(Diagnostic::new(
                &TOO_LONG_METADATA,
                Severity::Warn,
                format!(
                    "Summary is {len} chars long, recommended maximum is {MAX_SUMMARY_LEN}"
                ),
                span,
            ));
        }
    }
}

impl Lint for SummaryTooLong {
    fn metadata(&self) -> &'static LintMetadata {
        &TOO_LONG_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM058 name-in-summary
// =====================================================================

#[derive(Debug, Default)]
pub struct NameInSummary {
    diagnostics: Vec<Diagnostic>,
}

impl NameInSummary {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for NameInSummary {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let Some(name) = package_name(spec) else { return };
        for item in &spec.items {
            let rpm_spec::ast::SpecItem::Preamble(p) = item else { continue };
            let Some((value, span)) = summary_literal(p) else { continue };
            if contains_word(value, name) {
                self.diagnostics.push(Diagnostic::new(
                    &NAME_IN_SUMMARY_METADATA,
                    Severity::Allow,
                    format!("Summary repeats package name `{name}`"),
                    span,
                ));
            }
        }
    }
}

impl Lint for NameInSummary {
    fn metadata(&self) -> &'static LintMetadata {
        &NAME_IN_SUMMARY_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

/// Case-insensitive whole-word substring search. We don't want to flag
/// `Summary: postgresql-helper for postgres` on package `gres` — only
/// when the package name appears as a separated word.
fn contains_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let h = haystack.to_ascii_lowercase();
    let n = needle.to_ascii_lowercase();
    let bytes = h.as_bytes();
    let mut search_from = 0;
    while let Some(found) = h[search_from..].find(&n) {
        let start = search_from + found;
        let end = start + n.len();
        let prev_ok = start == 0
            || !bytes[start - 1].is_ascii_alphanumeric()
                && bytes[start - 1] != b'_';
        let next_ok = end == bytes.len()
            || !bytes[end].is_ascii_alphanumeric()
                && bytes[end] != b'_';
        if prev_ok && next_ok {
            return true;
        }
        search_from = start + 1;
    }
    false
}

// `TextSegment` is only used via the helper; suppress the unused import
// warning that fires when none of the rule bodies happen to mention it.
#[allow(dead_code)]
const _: Option<TextSegment> = None;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run_ends_dot(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = SummaryEndsWithDot::new();
        lint.set_source(src);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }
    fn run_capital(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = SummaryNotCapitalized::new();
        lint.set_source(src);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }
    fn run_long(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = SummaryTooLong::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }
    fn run_name(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = NameInSummary::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn rpm055_flags_trailing_dot() {
        let diags = run_ends_dot("Name: x\nSummary: A library.\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM055");
        assert!(!diags[0].suggestions.is_empty());
    }

    #[test]
    fn rpm055_silent_without_dot() {
        assert!(run_ends_dot("Name: x\nSummary: A library\n").is_empty());
    }

    #[test]
    fn rpm056_flags_lowercase_start() {
        let diags = run_capital("Name: x\nSummary: a library\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM056");
        assert!(!diags[0].suggestions.is_empty());
    }

    #[test]
    fn rpm056_silent_for_uppercase() {
        assert!(run_capital("Name: x\nSummary: A library\n").is_empty());
    }

    #[test]
    fn rpm056_silent_for_digit_start() {
        // Numerics aren't letters — out of scope.
        assert!(run_capital("Name: x\nSummary: 1st library\n").is_empty());
    }

    #[test]
    fn rpm057_flags_long_summary() {
        let long = "A".repeat(120);
        let src = format!("Name: x\nSummary: {long}\n");
        let diags = run_long(&src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM057");
    }

    #[test]
    fn rpm057_silent_for_short() {
        assert!(run_long("Name: x\nSummary: short\n").is_empty());
    }

    #[test]
    fn rpm058_flags_name_in_summary() {
        let diags = run_name("Name: foo\nSummary: foo command-line tool\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM058");
    }

    #[test]
    fn rpm058_silent_when_name_is_substring_only() {
        // `foo` is part of `foobar`, not a standalone word.
        assert!(run_name("Name: foo\nSummary: foobar utility\n").is_empty());
    }

    #[test]
    fn rpm058_silent_when_no_name() {
        assert!(run_name("Summary: utility\n").is_empty());
    }
}
