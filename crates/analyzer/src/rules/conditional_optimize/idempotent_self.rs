//! RPM086 `idempotent-in-expr` and RPM088 `self-comparison-in-expr`.
//!
//! Both walk parsed expression ASTs looking for operands that compare
//! equal to themselves (via `exprs_equiv`) under a boolean or
//! comparison operator.

use rpm_spec::ast::{CondExpr, Conditional, FilesContent, PreambleContent, Span, SpecItem};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::exprs_equiv;
use crate::visit::{self, Visit};

// =====================================================================
// RPM086 idempotent-in-expr
// =====================================================================

pub static IDEMPOTENT_METADATA: LintMetadata = LintMetadata {
    id: "RPM086",
    name: "idempotent-in-expr",
    description: "`X && X` / `X || X` repeats an operand — drop the duplicate.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct IdempotentInExpr {
    diagnostics: Vec<Diagnostic>,
}

impl IdempotentInExpr {
    pub fn new() -> Self {
        Self::default()
    }
}

fn find_idempotent_op<T>(ast: &rpm_spec::ast::ExprAst<T>) -> Option<&rpm_spec::ast::ExprAst<T>> {
    use rpm_spec::ast::{BinOp, ExprAst};
    match ast.peel_parens() {
        ExprAst::Binary {
            kind: BinOp::LogAnd | BinOp::LogOr,
            lhs,
            rhs,
            ..
        } => {
            if exprs_equiv(lhs, rhs) {
                return Some(ast);
            }
            find_idempotent_op(lhs).or_else(|| find_idempotent_op(rhs))
        }
        ExprAst::Not { inner, .. } | ExprAst::Paren { inner, .. } => find_idempotent_op(inner),
        ExprAst::Binary { lhs, rhs, .. } => {
            find_idempotent_op(lhs).or_else(|| find_idempotent_op(rhs))
        }
        _ => None,
    }
}

impl IdempotentInExpr {
    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for branch in &node.branches {
            let CondExpr::Parsed(ast) = &branch.expr else {
                continue;
            };
            if find_idempotent_op(ast).is_some() {
                self.diagnostics.push(
                    Diagnostic::new(
                        &IDEMPOTENT_METADATA,
                        Severity::Warn,
                        "`X && X` / `X || X` repeats an operand — simplify",
                        branch.data,
                    )
                    .with_suggestion(Suggestion::new(
                        "drop the duplicated operand",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

impl<'ast> Visit<'ast> for IdempotentInExpr {
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

impl Lint for IdempotentInExpr {
    fn metadata(&self) -> &'static LintMetadata {
        &IDEMPOTENT_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM088 self-comparison-in-expr
// =====================================================================

pub static SELF_COMPARISON_METADATA: LintMetadata = LintMetadata {
    id: "RPM088",
    name: "self-comparison-in-expr",
    description: "Comparison of an operand with itself has a fixed outcome.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct SelfComparisonInExpr {
    diagnostics: Vec<Diagnostic>,
}

impl SelfComparisonInExpr {
    pub fn new() -> Self {
        Self::default()
    }
}

fn find_self_comparison<T>(ast: &rpm_spec::ast::ExprAst<T>) -> Option<&'static str> {
    use rpm_spec::ast::{BinOp, ExprAst};
    match ast.peel_parens() {
        ExprAst::Binary { kind, lhs, rhs, .. } => {
            let cmp = matches!(
                kind,
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge
            );
            if cmp && exprs_equiv(lhs, rhs) {
                let verdict = match kind {
                    BinOp::Eq | BinOp::Le | BinOp::Ge => "always-true",
                    BinOp::Ne | BinOp::Lt | BinOp::Gt => "always-false",
                    _ => "always-constant",
                };
                return Some(verdict);
            }
            find_self_comparison(lhs).or_else(|| find_self_comparison(rhs))
        }
        ExprAst::Not { inner, .. } | ExprAst::Paren { inner, .. } => find_self_comparison(inner),
        _ => None,
    }
}

impl SelfComparisonInExpr {
    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for branch in &node.branches {
            let CondExpr::Parsed(ast) = &branch.expr else {
                continue;
            };
            if let Some(verdict) = find_self_comparison(ast) {
                self.diagnostics.push(
                    Diagnostic::new(
                        &SELF_COMPARISON_METADATA,
                        Severity::Warn,
                        format!("comparison of an operand with itself is {verdict}"),
                        branch.data,
                    )
                    .with_suggestion(Suggestion::new(
                        "replace the redundant comparison with the constant outcome",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

impl<'ast> Visit<'ast> for SelfComparisonInExpr {
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

impl Lint for SelfComparisonInExpr {
    fn metadata(&self) -> &'static LintMetadata {
        &SELF_COMPARISON_METADATA
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

    // ---- RPM086 idempotent-in-expr ----

    #[test]
    fn rpm086_flags_x_and_x() {
        let src = "Name: x\n%if 5 && 5\nLicense: MIT\n%endif\n";
        let diags = run(src, IdempotentInExpr::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM086");
    }

    #[test]
    fn rpm086_silent_for_distinct_operands() {
        let src = "Name: x\n%if 1 && 2\nLicense: MIT\n%endif\n";
        assert!(run(src, IdempotentInExpr::new()).is_empty());
    }

    // ---- RPM088 self-comparison-in-expr ----

    #[test]
    fn rpm088_flags_self_eq() {
        let src = "Name: x\n%if 5 == 5\nLicense: MIT\n%endif\n";
        let diags = run(src, SelfComparisonInExpr::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("always-true"));
    }

    #[test]
    fn rpm088_flags_self_lt() {
        let src = "Name: x\n%if 5 < 5\nLicense: MIT\n%endif\n";
        let diags = run(src, SelfComparisonInExpr::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("always-false"));
    }

    #[test]
    fn rpm088_silent_for_distinct_operands() {
        let src = "Name: x\n%if 5 == 4\nLicense: MIT\n%endif\n";
        assert!(run(src, SelfComparisonInExpr::new()).is_empty());
    }
}
