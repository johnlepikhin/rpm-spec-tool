//! RPM570 `commented-out-spec-code` — flag 3+ consecutive comment
//! lines whose text looks like commented-out spec syntax
//! (`#BuildRequires:`, `#%patch0`, `#%if`, …).
//!
//! Commented-out code rots quickly and tells reviewers nothing about
//! intent. Either remove it or rewrite as `%dnl` (no-expand comment)
//! with a rationale.

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM570",
    name: "commented-out-spec-code",
    description: "Three or more consecutive `#`-commented lines look like commented-out spec \
                  syntax — remove or replace with `%dnl` + rationale.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// Minimum run of commented-out lines to fire.
const RUN: usize = 3;

/// Three or more consecutive `#`-commented lines look like commented-out spec syntax — remove or replace with `%dnl` + rationale.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct CommentedOutSpecCode {
    diagnostics: Vec<Diagnostic>,
    source: std::sync::Arc<str>,
}

impl CommentedOutSpecCode {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for CommentedOutSpecCode {
    fn visit_spec(&mut self, _spec: &'ast SpecFile<Span>) {
        if self.source.is_empty() {
            return;
        }
        let mut run_start_byte = 0usize;
        let mut run_len = 0usize;
        let mut byte_cursor = 0usize;
        // Buffer the run; flush when streak breaks.
        let mut hits: Vec<Span> = Vec::new();
        // Split on `\n` and strip a trailing `\r` from each piece so
        // byte spans stay accurate on CRLF input. `str::lines()` hides
        // the `\r` and we would drift one byte per CRLF line.
        let total_len = self.source.len();
        for raw_line in self.source.split('\n') {
            let line_start = byte_cursor;
            let raw_len = raw_line.len();
            let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
            let consumed_newline = if byte_cursor + raw_len < total_len {
                1
            } else {
                0
            };
            byte_cursor += raw_len + consumed_newline;
            if looks_like_commented_spec_code(line) {
                if run_len == 0 {
                    run_start_byte = line_start;
                }
                run_len += 1;
            } else {
                if run_len >= RUN {
                    hits.push(Span::from_bytes(run_start_byte, line_start));
                }
                run_len = 0;
            }
        }
        if run_len >= RUN {
            hits.push(Span::from_bytes(run_start_byte, byte_cursor));
        }
        for span in hits {
            self.diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    "consecutive commented-out lines look like spec syntax — remove or replace \
                     with `%dnl` plus a rationale",
                    span,
                )
                .with_suggestion(Suggestion::new(
                    "delete the dead block or annotate it with `%dnl` and a comment explaining \
                     why it's kept",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

fn looks_like_commented_spec_code(line: &str) -> bool {
    let stripped = line.trim_start();
    let Some(rest) = stripped.strip_prefix('#') else {
        return false;
    };
    let body = rest.trim_start();
    if body.is_empty() {
        return false;
    }
    // `#TAG:` — commented preamble tag (Name, Version, Source0, ...).
    if let Some((head, _)) = body.split_once(':')
        && head.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && head.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !head.contains(' ')
    {
        return true;
    }
    // `#%macro` / `#%if` / `#%patch0` etc.
    body.starts_with('%')
}

impl Lint for CommentedOutSpecCode {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_source(&mut self, source: std::sync::Arc<str>) {
        self.source = source;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::test_support::run_lint;

    fn run(src: &str) -> Vec<Diagnostic> {
        run_lint::<CommentedOutSpecCode>(src)
    }

    #[test]
    fn flags_three_consecutive_commented_lines() {
        let src = "Name: x\n\
#BuildRequires: foo\n\
#BuildRequires: bar\n\
#%patch0 -p1\n\
Version: 1\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM570");
    }

    #[test]
    fn silent_for_short_run() {
        let src = "Name: x\n#BuildRequires: foo\n#BuildRequires: bar\nVersion: 1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_normal_comments() {
        let src = "Name: x\n\
# This explains a workaround.\n\
# Continued explanation.\n\
# Final line.\n\
Version: 1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn correct_span_under_crlf_input() {
        // CRLF input must produce a span whose `start_byte` lands on
        // the actual byte offset of the first commented-out line in
        // the source — not be shifted by the number of preceding
        // `\r\n` pairs.
        let src = "Name: x\r\n\
#BuildRequires: foo\r\n\
#BuildRequires: bar\r\n\
#%patch0 -p1\r\n\
Version: 1\r\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        let span = diags[0].primary_span;
        let expected_start = src
            .find("#BuildRequires: foo")
            .expect("comment block present");
        assert_eq!(
            span.start_byte, expected_start,
            "span.start_byte drifted under CRLF: got {} expected {}",
            span.start_byte, expected_start
        );
        // Span must end just before `Version:` (i.e. at the line_start
        // of the non-matching line).
        let expected_end = src.find("Version: 1").expect("Version line present");
        assert_eq!(
            span.end_byte, expected_end,
            "span.end_byte drifted under CRLF: got {} expected {}",
            span.end_byte, expected_end
        );
    }
}
