//! RPM435 `unnecessary-condition-parentheses` — flag `%if` expressions
//! whose parentheses add no precedence value.
//!
//! Cases caught:
//! - Double parens: `%if ((A))` → `%if A`.
//! - Parens wrapping a bare atom: `%if (A)` → `%if A`.
//!
//! Parens around a `Binary` / `Not` are *not* flagged — those may be
//! intentional precedence or readability cues.

use rpm_spec::ast::{
    CondExpr, Conditional, ExprAst, FilesContent, PreambleContent, Span, SpecItem,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM435",
    name: "unnecessary-condition-parentheses",
    description: "`%if` expression contains redundant parentheses — wrapping a single atom or \
                  nesting parens directly inside parens.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// `%if` expression contains redundant parentheses — wrapping a single atom or nesting parens directly inside parens.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct UnnecessaryConditionParens {
    diagnostics: Vec<Diagnostic>,
}

impl UnnecessaryConditionParens {
    pub fn new() -> Self {
        Self::default()
    }

    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for branch in &node.branches {
            let CondExpr::Parsed(ast) = &branch.expr else {
                continue;
            };
            if walk_ast_for_redundant_parens(ast) {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "`%if` expression has redundant parentheses (`(A)` around an atom or \
                         `((…))` nesting); drop them",
                        branch.data,
                    )
                    .with_suggestion(Suggestion::new(
                        "remove the redundant `(` / `)` pair",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

fn walk_ast_for_redundant_parens(ast: &ExprAst<Span>) -> bool {
    if is_redundant_paren(ast) {
        return true;
    }
    match ast {
        ExprAst::Paren { inner, .. } | ExprAst::Not { inner, .. } => {
            walk_ast_for_redundant_parens(inner)
        }
        ExprAst::Binary { lhs, rhs, .. } => {
            walk_ast_for_redundant_parens(lhs) || walk_ast_for_redundant_parens(rhs)
        }
        _ => false,
    }
}

/// `true` when `ast` is a `Paren` whose inner is either another `Paren`
/// (double parens) or a bare atom (parens around a single identifier /
/// literal / macro reference).
fn is_redundant_paren(ast: &ExprAst<Span>) -> bool {
    let ExprAst::Paren { inner, .. } = ast else {
        return false;
    };
    matches!(
        inner.as_ref(),
        ExprAst::Paren { .. }
            | ExprAst::Integer { .. }
            | ExprAst::String { .. }
            | ExprAst::Identifier { .. }
            | ExprAst::Macro { .. }
    )
}

impl<'ast> Visit<'ast> for UnnecessaryConditionParens {
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

impl Lint for UnnecessaryConditionParens {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::test_support::run_lint;

    fn run(src: &str) -> Vec<Diagnostic> {
        run_lint::<UnnecessaryConditionParens>(src)
    }

    #[test]
    fn flags_paren_around_atom() {
        let src = "Name: x\n%if (A)\nLicense: MIT\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM435");
    }

    #[test]
    fn flags_double_parens() {
        let src = "Name: x\n%if ((A && B))\nLicense: MIT\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn flags_paren_nested_in_subexpression() {
        let src = "Name: x\n%if X && (Y)\nLicense: MIT\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn silent_for_paren_around_binary() {
        // (A && B) parens may be intentional precedence cue.
        let src = "Name: x\n%if X || (A && B)\nLicense: MIT\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_plain_atom() {
        let src = "Name: x\n%if A\nLicense: MIT\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_plain_binary() {
        let src = "Name: x\n%if A && B\nLicense: MIT\n%endif\n";
        assert!(run(src).is_empty());
    }
}
