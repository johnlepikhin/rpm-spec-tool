//! RPM052 `trailing-whitespace` — strip trailing spaces and tabs from
//! source lines. Pure cosmetic, but cheap to fix and noisy in diffs.
//! Default severity is `Allow` so the rule doesn't shout — turn it on
//! per-project via `[lints] trailing-whitespace = "warn"` when desired.
//!
//! Like RPM051, this rule needs raw source bytes via
//! [`Lint::set_source`].

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Applicability, Diagnostic, Edit, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM052",
    name: "trailing-whitespace",
    description: "Trailing whitespace clutters diffs and serves no purpose.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct TrailingWhitespace {
    diagnostics: Vec<Diagnostic>,
    source: Option<String>,
}

impl TrailingWhitespace {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for TrailingWhitespace {
    fn visit_spec(&mut self, _spec: &'ast SpecFile<Span>) {
        let Some(source) = self.source.clone() else {
            return;
        };
        let mut line_start = 0usize;
        for (idx, byte) in source.bytes().enumerate() {
            if byte == b'\n' {
                self.check_line(line_start, idx, &source);
                line_start = idx + 1;
            }
        }
        if line_start < source.len() {
            self.check_line(line_start, source.len(), &source);
        }
    }
}

impl TrailingWhitespace {
    fn check_line(&mut self, start: usize, end: usize, source: &str) {
        let line = &source[start..end];
        // Find where the run of trailing whitespace begins.
        let mut ws_start = end;
        for (offset, byte) in line.bytes().enumerate().rev() {
            if byte == b' ' || byte == b'\t' {
                ws_start = start + offset;
            } else {
                break;
            }
        }
        if ws_start == end {
            return; // no trailing whitespace
        }
        if ws_start == start {
            // Entire line is whitespace — that's a blank line, leave it.
            // (The parser preserves blank lines for paragraphing.)
            return;
        }
        self.diagnostics.push(
            Diagnostic::new(
                &METADATA,
                Severity::Allow,
                "trailing whitespace",
                Span::from_bytes(ws_start, end),
            )
            .with_suggestion(Suggestion::new(
                "remove the trailing whitespace",
                vec![Edit::new(Span::from_bytes(ws_start, end), "")],
                Applicability::MachineApplicable,
            )),
        );
    }
}

impl Lint for TrailingWhitespace {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_source(&mut self, source: &str) {
        self.source = Some(source.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = TrailingWhitespace::new();
        lint.set_source(src);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_trailing_spaces() {
        let src = "Name: x   \nVersion: 1\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM052");
        // Auto-fix replaces the trailing whitespace with empty.
        assert!(diags[0].suggestions[0].edits[0].replacement.is_empty());
    }

    #[test]
    fn flags_trailing_tab() {
        let src = "Name: x\t\n";
        assert!(!run(src).is_empty());
    }

    #[test]
    fn silent_for_blank_line() {
        // Pure whitespace = blank line, intentional paragraphing.
        let src = "Name: x\n   \nVersion: 1\n";
        assert!(run(src).is_empty(), "{:?}", run(src));
    }

    #[test]
    fn silent_for_clean_lines() {
        let src = "Name: x\nVersion: 1\n";
        assert!(run(src).is_empty());
    }
}
