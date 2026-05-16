//! RPM051 `tab-indent` — flag lines that start with a tab character.
//! rpmlint reports this as `mixed-use-of-spaces-and-tabs` because tabs
//! interact badly with the preamble alignment column and look
//! different in every editor. Convention is 8-space indentation
//! matching rpm's preamble value column.
//!
//! This rule is the first real consumer of [`Lint::set_source`]: we
//! need the raw source bytes to scan line starts, which the AST
//! collapses away.

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Applicability, Diagnostic, Edit, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM051",
    name: "tab-indent",
    description: "Lines indented with tabs make alignment fragile; use spaces instead.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Width of one tab when expanded. 8 matches rpm's preamble value
/// alignment column convention.
const TAB_WIDTH: usize = 8;

#[derive(Debug, Default)]
pub struct TabIndent {
    diagnostics: Vec<Diagnostic>,
    source: Option<String>,
}

impl TabIndent {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for TabIndent {
    fn visit_spec(&mut self, _spec: &'ast SpecFile<Span>) {
        let Some(source) = self.source.clone() else {
            return;
        };
        let mut line_start = 0usize;
        for (idx, byte) in source.bytes().enumerate() {
            if byte == b'\n' {
                self.check_line(&source, line_start, idx);
                line_start = idx + 1;
            }
        }
        // Trailing line without a terminating newline.
        if line_start < source.len() {
            self.check_line(&source, line_start, source.len());
        }
    }
}

impl TabIndent {
    fn check_line(&mut self, source: &str, start: usize, end: usize) {
        let line = &source[start..end];
        // Count leading tabs (we only flag tabs that appear in the
        // indentation; a stray `\t` mid-line is unusual but harmless).
        let leading_tabs = line.bytes().take_while(|b| *b == b'\t').count();
        if leading_tabs == 0 {
            return;
        }
        let replacement = " ".repeat(TAB_WIDTH * leading_tabs);
        let edit_span = Span::from_bytes(start, start + leading_tabs);
        let line_span = Span::from_bytes(start, end);
        self.diagnostics.push(
            Diagnostic::new(
                &METADATA,
                Severity::Warn,
                format!("line starts with {leading_tabs} tab(s); use spaces for stable alignment"),
                line_span,
            )
            .with_suggestion(Suggestion::new(
                format!("replace leading tabs with {TAB_WIDTH} spaces each"),
                vec![Edit::new(edit_span, replacement)],
                Applicability::MachineApplicable,
            )),
        );
    }
}

impl Lint for TabIndent {
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
        let mut lint = TabIndent::new();
        lint.set_source(src);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_tab_indented_line() {
        let src = "Name: x\n\tRequires: foo\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM051");
        // Auto-fix should propose 8 spaces.
        let edits = &diags[0].suggestions[0].edits;
        assert_eq!(edits[0].replacement, " ".repeat(8));
    }

    #[test]
    fn silent_when_no_tabs() {
        let src = "Name: x\nRequires: foo\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_multiple_tabs() {
        let src = "Name: x\n\t\tindented\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].suggestions[0].edits[0].replacement, " ".repeat(16));
    }

    #[test]
    fn flags_tab_followed_by_text() {
        let src = "Name: x\n\tValue:\tfoo\n";
        // The mid-line `\t` between `Value:` and `foo` doesn't count —
        // only the leading tab does.
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        let leading = &diags[0].suggestions[0].edits[0];
        assert_eq!(leading.span.end_byte - leading.span.start_byte, 1);
    }
}
