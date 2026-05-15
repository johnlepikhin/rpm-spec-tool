//! RPM036 `macro-in-hash-comment` — `#`-style comments in RPM specs
//! **are** interpreted: rpm expands macros inside them before
//! discarding the comment. That means `# %{name}-debug` silently
//! executes whatever `%name` evaluates to and may even produce side
//! effects (`%(shell command)`, `%{lua:...}`). The only truly inert
//! comment form is `%dnl`.
//!
//! We flag any `# ...` comment whose text contains a `TextSegment::Macro`
//! and suggest escaping each `%` to `%%`. The actual edit needs to
//! touch the source bytes inside the comment span, so the rule keeps
//! the raw source string from [`Lint::set_source`] and constructs
//! `MachineApplicable` edits when available; otherwise the diagnostic
//! degrades to a `Manual` suggestion.

use rpm_spec::ast::{Comment, CommentStyle, Span, TextSegment};

use crate::diagnostic::{Applicability, Diagnostic, Edit, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM036",
    name: "macro-in-hash-comment",
    description:
        "`#` comments expand macros — escape each `%` to `%%` or use `%dnl` for a no-expand comment.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct MacroInHashComment {
    diagnostics: Vec<Diagnostic>,
    source: Option<String>,
}

impl MacroInHashComment {
    pub fn new() -> Self {
        Self::default()
    }
}

fn comment_contains_macro(node: &Comment<Span>) -> bool {
    node.text
        .segments
        .iter()
        .any(|seg| matches!(seg, TextSegment::Macro(_)))
}

/// Build edits that replace every standalone `%` inside the comment's
/// byte range with `%%`. Skips `%%` (already escaped) and `%dnl` —
/// though the latter shouldn't appear inside a `Hash` comment.
fn build_escape_edits(source: &str, span: Span) -> Vec<Edit> {
    let start = span.start_byte.min(source.len());
    let end = span.end_byte.min(source.len());
    if start >= end {
        return Vec::new();
    }
    let slice = &source[start..end];
    let bytes = slice.as_bytes();
    let mut edits = Vec::new();
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] != b'%' {
            idx += 1;
            continue;
        }
        // Skip `%%` (already escaped — advance past both bytes).
        if idx + 1 < bytes.len() && bytes[idx + 1] == b'%' {
            idx += 2;
            continue;
        }
        // Insert a `%` before this position to make the existing `%`
        // part of `%%`. The fixer applies edits in descending order,
        // so each insertion is independent.
        let abs = start + idx;
        edits.push(Edit::new(Span::from_bytes(abs, abs), "%".to_owned()));
        idx += 1;
    }
    edits
}

impl<'ast> Visit<'ast> for MacroInHashComment {
    fn visit_comment(&mut self, node: &'ast Comment<Span>) {
        if !matches!(node.style, CommentStyle::Hash) {
            return;
        }
        if !comment_contains_macro(node) {
            return;
        }
        let mut diag = Diagnostic::new(
            &METADATA,
            Severity::Warn,
            "macro reference inside `#` comment will be expanded by rpm",
            node.data,
        );
        let suggestion = match self.source.as_deref() {
            Some(source) => {
                let edits = build_escape_edits(source, node.data);
                if edits.is_empty() {
                    Suggestion::new(
                        "escape each `%` to `%%` or change `#` to `%dnl`",
                        Vec::new(),
                        Applicability::Manual,
                    )
                } else {
                    Suggestion::new(
                        "escape each `%` to `%%` so rpm leaves the text alone",
                        edits,
                        Applicability::MachineApplicable,
                    )
                }
            }
            None => Suggestion::new(
                "escape each `%` to `%%` or change `#` to `%dnl`",
                Vec::new(),
                Applicability::Manual,
            ),
        };
        diag = diag.with_suggestion(suggestion);
        self.diagnostics.push(diag);
    }
}

impl Lint for MacroInHashComment {
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
        let mut lint = MacroInHashComment::new();
        lint.set_source(src);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_macro_in_hash_comment() {
        let src = "Name: x\n# %{name} is the package\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM036");
        assert!(!diags[0].suggestions.is_empty());
        // Edits should turn `%` into `%%`.
        let edits = &diags[0].suggestions[0].edits;
        assert!(!edits.is_empty(), "expected escape edits");
    }

    #[test]
    fn silent_for_plain_comment() {
        let src = "Name: x\n# plain text without macros\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_dnl_comment() {
        // %dnl comments don't expand macros, so they're safe.
        let src = "Name: x\n%dnl %{name} is the package\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_inline_macro_call() {
        // `%(shell)` would silently run a subshell.
        let src = "Name: x\n# safe: %(echo hi)\n";
        assert!(!run(src).is_empty());
    }
}
