//! RPM105 `inverted-if-else` — `%if !X foo %else bar %endif` reads
//! more naturally when the negation is removed and the branches are
//! swapped.

use rpm_spec::ast::{
    CondExpr, CondKind, Conditional, FilesContent, PreambleContent, Span, SpecItem,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

pub static INVERTED_IF_ELSE_METADATA: LintMetadata = LintMetadata {
    id: "RPM105",
    name: "inverted-if-else",
    description: "`%if !X foo %else bar %endif` reads more naturally when the negation is removed and \
         the branches are swapped.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct InvertedIfElse {
    diagnostics: Vec<Diagnostic>,
}

impl InvertedIfElse {
    pub fn new() -> Self {
        Self::default()
    }
}

/// `true` when the head expression is `!something` — either a
/// parsed `Not` node or a `Raw` literal starting with `!` (and not
/// `!=`, which is the inequality operator).
fn head_is_negation<T>(expr: &CondExpr<T>) -> bool {
    match expr {
        CondExpr::Parsed(ast) => matches!(ast.peel_parens(), rpm_spec::ast::ExprAst::Not { .. }),
        CondExpr::Raw(text) => match text.literal_str() {
            Some(lit) => {
                let trimmed = lit.trim_start();
                trimmed.starts_with('!') && !trimmed.starts_with("!=")
            }
            None => false,
        },
        _ => false,
    }
}

impl InvertedIfElse {
    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        // Pattern: exactly one branch (no `%elif`), a non-empty `%else`,
        // and the `%if` head is `!X`.
        if node.branches.len() != 1 || node.otherwise.is_none() {
            return;
        }
        let branch = &node.branches[0];
        if !head_is_negation(&branch.expr) {
            return;
        }
        // Only plain `%if`; arch/os negations have dedicated keywords
        // (`%ifnarch`/`%ifnos`), so RPM082 handles those cases.
        if !matches!(branch.kind, CondKind::If) {
            return;
        }
        self.diagnostics.push(
            Diagnostic::new(
                &INVERTED_IF_ELSE_METADATA,
                Severity::Warn,
                "`%if !X ... %else ... %endif` — remove the negation and swap the branch bodies",
                node.data,
            )
            .with_suggestion(Suggestion::new(
                "drop the leading `!` and swap the `%if` body with the `%else` body",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

impl<'ast> Visit<'ast> for InvertedIfElse {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        self.check(node);
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        self.check(node);
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        self.check(node);
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for InvertedIfElse {
    fn metadata(&self) -> &'static LintMetadata {
        &INVERTED_IF_ELSE_METADATA
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
    fn rpm105_flags_negated_if_with_else() {
        let src = "Name: x\n%if !X\nLicense: MIT\n%else\nLicense: GPL\n%endif\n";
        let diags = run(src, InvertedIfElse::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM105");
    }

    #[test]
    fn rpm105_silent_for_non_negated_if() {
        let src = "Name: x\n%if X\nLicense: MIT\n%else\nLicense: GPL\n%endif\n";
        assert!(run(src, InvertedIfElse::new()).is_empty());
    }

    #[test]
    fn rpm105_silent_when_no_else() {
        let src = "Name: x\n%if !X\nLicense: MIT\n%endif\n";
        assert!(run(src, InvertedIfElse::new()).is_empty());
    }

    #[test]
    fn rpm105_silent_for_not_equal_op() {
        // `X != 1` starts with `X`, not `!`. Should not trigger.
        let src = "Name: x\n%if X != 1\nLicense: MIT\n%else\nLicense: GPL\n%endif\n";
        assert!(run(src, InvertedIfElse::new()).is_empty());
    }
}
