//! RPM434 `negated-comparison-simplify` — flag `%if !(X OP Y)` where
//! the operator can be flipped to drop the outer negation.
//!
//! `!(X >= 8)` ↔ `X < 8`. The rewriting table:
//! - `!=` ↔ `==`
//! - `<` ↔ `>=`
//! - `<=` ↔ `>`
//!
//! Operates purely on parsed expressions; macro-tainted operands don't
//! affect the rewrite (the operator flip is structural).

use rpm_spec::ast::{
    BinOp, CondExpr, Conditional, ExprAst, FilesContent, PreambleContent, Span, SpecItem,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM434",
    name: "negated-comparison-simplify",
    description: "`!(X OP Y)` can be rewritten by flipping the comparison operator (e.g. \
                  `!(X >= 8)` → `X < 8`).",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// `!(X OP Y)` can be rewritten by flipping the comparison operator (e.g. `!(X >= 8)` → `X < 8`).
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct NegatedComparisonSimplify {
    diagnostics: Vec<Diagnostic>,
}

impl NegatedComparisonSimplify {
    pub fn new() -> Self {
        Self::default()
    }

    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for branch in &node.branches {
            let CondExpr::Parsed(ast) = &branch.expr else {
                continue;
            };
            if walk_for_negated_compare(ast) {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "`%if` expression negates a comparison (`!(X OP Y)`); flip the operator \
                         and drop the `!`",
                        branch.data,
                    )
                    .with_suggestion(Suggestion::new(
                        "use the inverse comparison operator (`<` ↔ `>=`, `<=` ↔ `>`, `==` ↔ `!=`)",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

fn walk_for_negated_compare(ast: &ExprAst<Span>) -> bool {
    if let ExprAst::Not { inner, .. } = ast
        && is_comparison(inner.as_ref().peel_parens())
    {
        return true;
    }
    match ast {
        ExprAst::Paren { inner, .. } | ExprAst::Not { inner, .. } => {
            walk_for_negated_compare(inner)
        }
        ExprAst::Binary { lhs, rhs, .. } => {
            walk_for_negated_compare(lhs) || walk_for_negated_compare(rhs)
        }
        _ => false,
    }
}

fn is_comparison(ast: &ExprAst<Span>) -> bool {
    matches!(
        ast,
        ExprAst::Binary {
            kind: BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge,
            ..
        }
    )
}

impl<'ast> Visit<'ast> for NegatedComparisonSimplify {
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

impl Lint for NegatedComparisonSimplify {
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
        run_lint::<NegatedComparisonSimplify>(src)
    }

    #[test]
    fn flags_negated_ge() {
        let src = "Name: x\n%if !(X >= 8)\nLicense: MIT\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM434");
    }

    #[test]
    fn flags_negated_eq() {
        let src = "Name: x\n%if !(X == 5)\nLicense: MIT\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn flags_negated_inside_compound() {
        let src = "Name: x\n%if A && !(X < 10)\nLicense: MIT\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_negated_boolean() {
        // `!A` (negation of identifier) is fine — only comparisons get rewritten.
        let src = "Name: x\n%if !A\nLicense: MIT\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_unnegated_comparison() {
        let src = "Name: x\n%if X >= 8\nLicense: MIT\n%endif\n";
        assert!(run(src).is_empty());
    }
}
