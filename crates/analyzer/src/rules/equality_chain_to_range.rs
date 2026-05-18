//! RPM443 `equality-chain-to-range` — flag long `||`-chains of integer
//! equality comparisons that form a contiguous range.
//!
//! `%if X == 8 || X == 9 || X == 10` is just `%if X >= 8 && X <= 10`,
//! which scales better as the range grows.
//!
//! Only fires when:
//! - Every disjunct is `X == INT` (or `INT == X`) with the same `X`.
//! - The integer values form a contiguous range with at least 3 entries
//!   (2-element chains aren't shorter in range form).

use std::collections::BTreeSet;

use rpm_spec::ast::{
    BinOp, CondExpr, Conditional, ExprAst, FilesContent, PreambleContent, Span, SpecItem,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{exprs_equiv, flatten_or};
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM443",
    name: "equality-chain-to-range",
    description: "`X == N1 || X == N2 || X == N3 …` over a contiguous integer range — rewrite \
                  as `X >= MIN && X <= MAX`.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// `X == N1 || X == N2 || X == N3 …` over a contiguous integer range — rewrite as `X >= MIN && X <= MAX`.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct EqualityChainToRange {
    diagnostics: Vec<Diagnostic>,
}

impl EqualityChainToRange {
    pub fn new() -> Self {
        Self::default()
    }

    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for branch in &node.branches {
            let CondExpr::Parsed(ast) = &branch.expr else {
                continue;
            };
            let or_operands = flatten_or(ast.as_ref().peel_parens());
            if or_operands.len() < 3 {
                continue;
            }
            let Some((min, max)) = detect_equality_range(&or_operands) else {
                continue;
            };
            self.diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    format!(
                        "`%if` chains {n} equality comparisons over the contiguous range \
                         [{min}, {max}]; rewrite as `X >= {min} && X <= {max}`",
                        n = or_operands.len()
                    ),
                    branch.data,
                )
                .with_suggestion(Suggestion::new(
                    "use a range comparison instead of an equality chain",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

/// Returns `Some((min, max))` when every operand is `X == INT` with the
/// same `X` (any expression structurally identical across operands) and
/// the integer values form a contiguous range.
fn detect_equality_range(operands: &[&ExprAst<Span>]) -> Option<(i64, i64)> {
    let mut anchor: Option<&ExprAst<Span>> = None;
    let mut values: BTreeSet<i64> = BTreeSet::new();
    for op in operands {
        let (var, n) = extract_eq_int(op)?;
        match anchor {
            None => anchor = Some(var),
            Some(prev) => {
                if !exprs_equiv(prev, var) {
                    return None;
                }
            }
        }
        if !values.insert(n) {
            return None; // duplicate value — RPM104 territory
        }
    }
    let min = *values.iter().next()?;
    let max = *values.iter().next_back()?;
    if (max as i128 - min as i128 + 1) as usize != values.len() {
        return None; // not contiguous
    }
    Some((min, max))
}

/// Match `X == INT` or `INT == X`; returns `(var_expr, int_value)`.
fn extract_eq_int(ast: &ExprAst<Span>) -> Option<(&ExprAst<Span>, i64)> {
    let ExprAst::Binary {
        kind: BinOp::Eq,
        lhs,
        rhs,
        ..
    } = ast.peel_parens()
    else {
        return None;
    };
    if let ExprAst::Integer { value, .. } = rhs.as_ref().peel_parens() {
        return Some((lhs.as_ref().peel_parens(), *value));
    }
    if let ExprAst::Integer { value, .. } = lhs.as_ref().peel_parens() {
        return Some((rhs.as_ref().peel_parens(), *value));
    }
    None
}

impl<'ast> Visit<'ast> for EqualityChainToRange {
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

impl Lint for EqualityChainToRange {
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
        run_lint::<EqualityChainToRange>(src)
    }

    #[test]
    fn flags_three_term_contiguous_chain() {
        let src = "Name: x\n%if X == 8 || X == 9 || X == 10\nLicense: MIT\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM443");
        assert!(diags[0].message.contains("[8, 10]"));
    }

    #[test]
    fn flags_five_term_chain() {
        let src =
            "Name: x\n%if X == 1 || X == 2 || X == 3 || X == 4 || X == 5\nLicense: MIT\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_two_term_chain() {
        let src = "Name: x\n%if X == 8 || X == 9\nLicense: MIT\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_non_contiguous() {
        let src = "Name: x\n%if X == 8 || X == 10 || X == 12\nLicense: MIT\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_different_variables() {
        let src = "Name: x\n%if X == 8 || Y == 9 || Z == 10\nLicense: MIT\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_non_equality() {
        let src = "Name: x\n%if X >= 8 && X <= 10\nLicense: MIT\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn handles_int_on_left_side() {
        let src = "Name: x\n%if 8 == X || 9 == X || 10 == X\nLicense: MIT\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }
}
