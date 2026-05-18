//! RPM101 `absorption-in-expr` — boolean absorption:
//! `A || (A && B)` reduces to `A`; `A && (A || B)` reduces to `A`.

use rpm_spec::ast::{CondExpr, Conditional, FilesContent, PreambleContent, Span, SpecItem};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::exprs_equiv;
use crate::visit::{self, Visit};

pub static ABSORPTION_METADATA: LintMetadata = LintMetadata {
    id: "RPM101",
    name: "absorption-in-expr",
    description: "Boolean absorption: `A || (A && B)` reduces to `A`; `A && (A || B)` reduces to `A`.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct AbsorptionInExpr {
    diagnostics: Vec<Diagnostic>,
}

impl AbsorptionInExpr {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Detect any absorption pattern anywhere in the AST:
/// - `A || (A && B)` / `(A && B) || A`
/// - `A && (A || B)` / `(A || B) && A`
fn has_absorption<T>(ast: &rpm_spec::ast::ExprAst<T>) -> bool {
    use rpm_spec::ast::{BinOp, ExprAst};
    let bare = ast.peel_parens();
    if let ExprAst::Binary { kind, lhs, rhs, .. } = bare {
        let lhs_inner = lhs.as_ref().peel_parens();
        let rhs_inner = rhs.as_ref().peel_parens();
        match kind {
            BinOp::LogOr => {
                // A || (A && B)
                if let ExprAst::Binary {
                    kind: BinOp::LogAnd,
                    lhs: l2,
                    rhs: r2,
                    ..
                } = rhs_inner
                    && (exprs_equiv(lhs, l2) || exprs_equiv(lhs, r2))
                {
                    return true;
                }
                if let ExprAst::Binary {
                    kind: BinOp::LogAnd,
                    lhs: l2,
                    rhs: r2,
                    ..
                } = lhs_inner
                    && (exprs_equiv(rhs, l2) || exprs_equiv(rhs, r2))
                {
                    return true;
                }
            }
            BinOp::LogAnd => {
                // A && (A || B)
                if let ExprAst::Binary {
                    kind: BinOp::LogOr,
                    lhs: l2,
                    rhs: r2,
                    ..
                } = rhs_inner
                    && (exprs_equiv(lhs, l2) || exprs_equiv(lhs, r2))
                {
                    return true;
                }
                if let ExprAst::Binary {
                    kind: BinOp::LogOr,
                    lhs: l2,
                    rhs: r2,
                    ..
                } = lhs_inner
                    && (exprs_equiv(rhs, l2) || exprs_equiv(rhs, r2))
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    // Recurse into sub-expressions.
    match bare {
        ExprAst::Binary { lhs, rhs, .. } => has_absorption(lhs) || has_absorption(rhs),
        ExprAst::Not { inner, .. } => has_absorption(inner),
        _ => false,
    }
}

impl AbsorptionInExpr {
    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for branch in &node.branches {
            let CondExpr::Parsed(ast) = &branch.expr else {
                continue;
            };
            if has_absorption(ast) {
                self.diagnostics.push(
                    Diagnostic::new(
                        &ABSORPTION_METADATA,
                        Severity::Warn,
                        "boolean absorption: simplify `A || (A && B)` → `A` \
                         (or `A && (A || B)` → `A`)",
                        branch.data,
                    )
                    .with_suggestion(Suggestion::new(
                        "drop the absorbed sub-expression",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

impl<'ast> Visit<'ast> for AbsorptionInExpr {
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

impl Lint for AbsorptionInExpr {
    fn metadata(&self) -> &'static LintMetadata {
        &ABSORPTION_METADATA
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
    fn rpm101_flags_or_absorption() {
        // `5 || (5 && 6)` → can be reduced to `5`. Use integer
        // literals to avoid macro bail-out; absorption is a pure
        // boolean-algebra reduction.
        let src = "Name: x\n%if 5 || (5 && 6)\nLicense: MIT\n%endif\n";
        let diags = run(src, AbsorptionInExpr::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM101");
    }

    #[test]
    fn rpm101_flags_and_absorption() {
        let src = "Name: x\n%if 5 && (5 || 6)\nLicense: MIT\n%endif\n";
        let diags = run(src, AbsorptionInExpr::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn rpm101_silent_for_independent_operands() {
        let src = "Name: x\n%if 5 || (6 && 7)\nLicense: MIT\n%endif\n";
        assert!(run(src, AbsorptionInExpr::new()).is_empty());
    }
}
