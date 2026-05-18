//! RPM081 `empty-else-drop` — `%if X FOO %else %endif` → drop the
//! empty `%else` clause.

use rpm_spec::ast::{Conditional, FilesContent, PreambleContent, Span, SpecItem};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

pub static EMPTY_ELSE_METADATA: LintMetadata = LintMetadata {
    id: "RPM081",
    name: "empty-else-drop",
    description: "`%else` clause has no content; drop the empty `%else`.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct EmptyElseDrop {
    diagnostics: Vec<Diagnostic>,
}

impl EmptyElseDrop {
    pub fn new() -> Self {
        Self::default()
    }

    fn emit(&mut self, anchor: Span) {
        self.diagnostics.push(
            Diagnostic::new(
                &EMPTY_ELSE_METADATA,
                Severity::Warn,
                "`%else` clause is empty (only blanks/comments) — drop it",
                anchor,
            )
            .with_suggestion(Suggestion::new(
                "delete the `%else` line and its empty body",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

impl<'ast> Visit<'ast> for EmptyElseDrop {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        // Fire only when at least one branch has real content,
        // otherwise RPM073 (empty-conditional-branch) handles the
        // whole block.
        let has_real_branch = node.branches.iter().any(|b| {
            !b.body
                .iter()
                .all(|i| matches!(i, SpecItem::Blank | SpecItem::Comment(_)))
        });
        if has_real_branch
            && let Some(other) = &node.otherwise
            && other
                .iter()
                .all(|i| matches!(i, SpecItem::Blank | SpecItem::Comment(_)))
        {
            self.emit(node.data);
        }
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        let has_real_branch = node.branches.iter().any(|b| {
            !b.body
                .iter()
                .all(|i| matches!(i, PreambleContent::Blank | PreambleContent::Comment(_)))
        });
        if has_real_branch
            && let Some(other) = &node.otherwise
            && other
                .iter()
                .all(|i| matches!(i, PreambleContent::Blank | PreambleContent::Comment(_)))
        {
            self.emit(node.data);
        }
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        let has_real_branch = node.branches.iter().any(|b| {
            !b.body
                .iter()
                .all(|i| matches!(i, FilesContent::Blank | FilesContent::Comment(_)))
        });
        if has_real_branch
            && let Some(other) = &node.otherwise
            && other
                .iter()
                .all(|i| matches!(i, FilesContent::Blank | FilesContent::Comment(_)))
        {
            self.emit(node.data);
        }
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for EmptyElseDrop {
    fn metadata(&self) -> &'static LintMetadata {
        &EMPTY_ELSE_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::Diagnostic;
    use crate::session::parse;

    fn run<L: Lint>(src: &str, mut lint: L) -> Vec<Diagnostic> {
        let outcome = parse(src);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn rpm081_flags_empty_else() {
        let src = "Name: x\n%if 1\nVersion: 1\n%else\n%endif\n";
        let diags = run(src, EmptyElseDrop::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM081");
    }

    #[test]
    fn rpm081_silent_when_else_has_content() {
        let src = "Name: x\n%if 1\nVersion: 1\n%else\nVersion: 2\n%endif\n";
        assert!(run(src, EmptyElseDrop::new()).is_empty());
    }

    #[test]
    fn rpm081_silent_when_no_else() {
        let src = "Name: x\n%if 1\nVersion: 1\n%endif\n";
        assert!(run(src, EmptyElseDrop::new()).is_empty());
    }

    #[test]
    fn rpm081_silent_when_all_branches_empty() {
        // RPM073 (empty-conditional-branch) handles this case.
        let src = "Name: x\n%if 0\n%else\n%endif\n";
        assert!(run(src, EmptyElseDrop::new()).is_empty());
    }
}
