//! RPM433 `condition-common-disjunct-factor` — flag `%if` expressions
//! shaped like `(A || B) && (A || C)` whose top-level `&&` clauses
//! share a common subterm.
//!
//! Such an expression factors to `A || (B && C)`, which is usually
//! shorter and easier to read. Detection is purely syntactic on the
//! AST shape — sibling to RPM432 (`condition-common-factor`), which
//! works on the normalised DNF form.

use rpm_spec::ast::{
    BinOp, CondExpr, Conditional, ExprAst, FilesContent, PreambleContent, Span, SpecItem,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{exprs_equiv, flatten_or};
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM433",
    name: "condition-common-disjunct-factor",
    description: "`(A || B) && (A || C)` style `%if` expression — every top-level `&&` clause \
                  shares a common disjunct; factor it out to `A || (B && C)`.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// `(A || B) && (A || C)` style `%if` expression — every top-level `&&` clause shares a common disjunct; factor it out to `A || (B && C)`.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct ConditionCommonDisjunctFactor {
    diagnostics: Vec<Diagnostic>,
}

impl ConditionCommonDisjunctFactor {
    pub fn new() -> Self {
        Self::default()
    }

    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for branch in &node.branches {
            let CondExpr::Parsed(ast) = &branch.expr else {
                continue;
            };
            let conjuncts = flatten_and(ast.as_ref().peel_parens());
            if conjuncts.len() < 2 {
                continue;
            }
            // Each conjunct must itself be a `||`-chain (otherwise
            // there's no disjunct to factor out).
            let disjunct_lists: Vec<Vec<&ExprAst<Span>>> = conjuncts
                .iter()
                .map(|c| flatten_or(c.peel_parens()))
                .collect();
            if disjunct_lists.iter().any(|d| d.len() < 2) {
                continue;
            }
            // Common subterm — present in EVERY conjunct's `||`-list.
            let first = &disjunct_lists[0];
            for candidate in first {
                if disjunct_lists[1..]
                    .iter()
                    .all(|other| other.iter().any(|x| exprs_equiv(candidate, x)))
                {
                    self.diagnostics.push(
                        Diagnostic::new(
                            &METADATA,
                            Severity::Warn,
                            "`%if` expression's `&&` clauses share a common disjunct — factor it \
                             out (`(A || B) && (A || C)` → `A || (B && C)`)",
                            branch.data,
                        )
                        .with_suggestion(Suggestion::new(
                            "rewrite as `factor || (rest_1 && rest_2 && …)`",
                            Vec::new(),
                            Applicability::Manual,
                        )),
                    );
                    break;
                }
            }
        }
    }
}

fn flatten_and(ast: &ExprAst<Span>) -> Vec<&ExprAst<Span>> {
    let mut out = Vec::new();
    fn rec<'a>(ast: &'a ExprAst<Span>, out: &mut Vec<&'a ExprAst<Span>>) {
        match ast.peel_parens() {
            ExprAst::Binary {
                kind: BinOp::LogAnd,
                lhs,
                rhs,
                ..
            } => {
                rec(lhs, out);
                rec(rhs, out);
            }
            other => out.push(other),
        }
    }
    rec(ast, &mut out);
    out
}

impl<'ast> Visit<'ast> for ConditionCommonDisjunctFactor {
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

impl Lint for ConditionCommonDisjunctFactor {
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
        run_lint::<ConditionCommonDisjunctFactor>(src)
    }

    #[test]
    fn flags_two_clause_common_disjunct() {
        let src = "Name: x\n%if (A || B) && (A || C)\nLicense: MIT\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM433");
    }

    #[test]
    fn flags_three_clause_common_disjunct() {
        let src = "Name: x\n%if (A || B) && (A || C) && (A || D)\nLicense: MIT\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_no_common_disjunct() {
        let src = "Name: x\n%if (A || B) && (C || D)\nLicense: MIT\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_pure_and_chain() {
        // No `||` at all — nothing to factor.
        let src = "Name: x\n%if A && B && C\nLicense: MIT\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_pure_or_chain() {
        let src = "Name: x\n%if A || B || C\nLicense: MIT\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_one_clause_is_a_singleton() {
        // `A && (B || C)` — top-level has one disjunct (`A`) and one
        // `||`-chain. No factoring opportunity at this shape.
        let src = "Name: x\n%if A && (B || C)\nLicense: MIT\n%endif\n";
        assert!(run(src).is_empty());
    }
}
