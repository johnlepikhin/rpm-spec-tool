//! Interval-analysis lints over `%if` expressions (Phase 7d).
//!
//! - RPM102 `inequality-redundancy` — `X >= 8 && X >= 6`: the
//!   weaker constraint is subsumed by the stronger one.
//! - RPM103 `inequality-contradiction` — `X >= 10 && X < 5`: the
//!   intersection is empty, so the whole guard is dead.
//!
//! Both rules walk a top-level `&&`-chain in the parsed expression
//! AST, collect `(lhs, op, integer-rhs)` constraints whose `lhs` is
//! structurally identical, then ask whether any pair is redundant
//! or contradictory.
//!
//! ## Conservative scope
//!
//! - **Integer rhs only.** Comparisons against strings/macros/
//!   identifiers are skipped — their order isn't statically known.
//! - **Same lhs only.** Constraints on different lhs sides are
//!   independent; the rules don't reason across them.
//! - **Unconditional macro lhs.** When `lhs` references `%{name}`
//!   without a `?` (i.e. fails-on-undefined), we still group by
//!   text — RPM evaluates such expressions eagerly, so the analysis
//!   is sound. The user gets the same warning whether or not
//!   `%{name}` is defined.
//! - **No nested `||`/`!` traversal.** Inside a top-level `&&`-chain
//!   we only look at direct relational children. `(X >= 8) && (X
//!   >= 6 || Y)` will not flag the `X >= 6` because it's beneath an
//!   `||` — that case has different semantics.
//! - **Auto-fix:** Manual for RPM102; none for RPM103 (whole block
//!   is dead — author decides what to do).

use rpm_spec::ast::{
    BinOp, CondExpr, Conditional, ExprAst, FilesContent, PreambleContent, Span, SpecItem,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::exprs_equiv;
use crate::visit::{self, Visit};

// =====================================================================
// Shared infrastructure
// =====================================================================

/// One relational constraint with an integer literal on the right.
///
/// `lhs` is borrowed from the expression AST; lifetimes are tied to
/// the visited [`Conditional`].
struct IntConstraint<'a> {
    lhs: &'a ExprAst<Span>,
    op: BinOp,
    value: i64,
}

/// Walk `ast` (already peeled of `Paren`) gathering operands of a
/// top-level `&&`-chain. Stops descending at any non-`LogAnd` node,
/// preserving the conservative top-level-only contract.
fn flatten_and_chain<'a>(ast: &'a ExprAst<Span>, out: &mut Vec<&'a ExprAst<Span>>) {
    match ast.peel_parens() {
        ExprAst::Binary {
            kind: BinOp::LogAnd,
            lhs,
            rhs,
            ..
        } => {
            flatten_and_chain(lhs, out);
            flatten_and_chain(rhs, out);
        }
        other => out.push(other),
    }
}

/// `Some` if `ast` is a relational comparison `lhs OP integer-rhs`.
/// The op is one of `Lt`/`Gt`/`Le`/`Ge`/`Eq`/`Ne`; anything else
/// returns `None`. `lhs` is returned by reference so callers can use
/// [`exprs_equiv`] to group constraints with identical lhs.
fn extract_int_constraint(ast: &ExprAst<Span>) -> Option<IntConstraint<'_>> {
    if let ExprAst::Binary { kind, lhs, rhs, .. } = ast.peel_parens()
        && matches!(
            kind,
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge | BinOp::Eq | BinOp::Ne
        )
        && let ExprAst::Integer { value, .. } = rhs.peel_parens()
    {
        return Some(IntConstraint {
            lhs,
            op: *kind,
            value: *value,
        });
    }
    None
}

/// True when `a` is at least as strong as `b` — i.e. every value
/// satisfying `a` also satisfies `b`. Both must constrain the same
/// (already-grouped) lhs. Used to flag `b` as redundant in an
/// `&&`-chain.
fn implies(a: &IntConstraint<'_>, b: &IntConstraint<'_>) -> bool {
    // Equality and inequality don't combine sensibly with relational
    // implications in v1; treat them as opaque.
    if matches!(a.op, BinOp::Eq | BinOp::Ne) || matches!(b.op, BinOp::Eq | BinOp::Ne) {
        return false;
    }
    // Normalise direction. `>=`/`>` say "X is at least V";
    // `<=`/`<` say "X is at most V". Cross-direction never implies.
    let a_lower = matches!(a.op, BinOp::Ge | BinOp::Gt);
    let b_lower = matches!(b.op, BinOp::Ge | BinOp::Gt);
    if a_lower != b_lower {
        return false;
    }
    if a_lower {
        // Lower bound: `X >= 8` (a) implies `X >= 6` (b) — a is
        // tighter when a.value >= b.value and a.op is at least as
        // strict.
        let a_min = match a.op {
            BinOp::Ge => a.value,
            BinOp::Gt => a.value + 1,
            _ => return false,
        };
        let b_min = match b.op {
            BinOp::Ge => b.value,
            BinOp::Gt => b.value + 1,
            _ => return false,
        };
        a_min >= b_min
    } else {
        // Upper bound: `X <= 5` implies `X <= 10`.
        let a_max = match a.op {
            BinOp::Le => a.value,
            BinOp::Lt => a.value - 1,
            _ => return false,
        };
        let b_max = match b.op {
            BinOp::Le => b.value,
            BinOp::Lt => b.value - 1,
            _ => return false,
        };
        a_max <= b_max
    }
}

/// `Some(true)` when `a` and `b` together have an empty solution
/// set on the same lhs. `None` for "can't tell".
fn contradicts(a: &IntConstraint<'_>, b: &IntConstraint<'_>) -> Option<bool> {
    if matches!(a.op, BinOp::Eq) && matches!(b.op, BinOp::Eq) {
        return Some(a.value != b.value);
    }
    // Lower-bound vs upper-bound: empty if min > max.
    let lower = if matches!(a.op, BinOp::Ge | BinOp::Gt) {
        Some(a)
    } else if matches!(b.op, BinOp::Ge | BinOp::Gt) {
        Some(b)
    } else {
        None
    };
    let upper = if matches!(a.op, BinOp::Le | BinOp::Lt) {
        Some(a)
    } else if matches!(b.op, BinOp::Le | BinOp::Lt) {
        Some(b)
    } else {
        None
    };
    let (lo, hi) = (lower?, upper?);
    let lo_min = match lo.op {
        BinOp::Ge => lo.value,
        BinOp::Gt => lo.value + 1,
        _ => return None,
    };
    let hi_max = match hi.op {
        BinOp::Le => hi.value,
        BinOp::Lt => hi.value - 1,
        _ => return None,
    };
    Some(lo_min > hi_max)
}

/// Inspect the top-level `&&`-chain of `ast` and return the verdict
/// — `redundancy` is `true` if any pair is redundant, `contradiction`
/// is `true` if any pair is mutually exclusive. Both can fire at
/// once on pathological inputs.
fn analyse(ast: &ExprAst<Span>) -> (bool, bool) {
    let mut operands = Vec::new();
    flatten_and_chain(ast, &mut operands);
    let constraints: Vec<IntConstraint<'_>> = operands
        .iter()
        .filter_map(|o| extract_int_constraint(o))
        .collect();
    let mut redundant = false;
    let mut contradicts_any = false;
    for i in 0..constraints.len() {
        for j in (i + 1)..constraints.len() {
            let a = &constraints[i];
            let b = &constraints[j];
            if !exprs_equiv(a.lhs, b.lhs) {
                continue;
            }
            if implies(a, b) || implies(b, a) {
                redundant = true;
            }
            if matches!(contradicts(a, b), Some(true)) {
                contradicts_any = true;
            }
        }
    }
    (redundant, contradicts_any)
}

// =====================================================================
// RPM102 inequality-redundancy
// =====================================================================

pub static REDUNDANCY_METADATA: LintMetadata = LintMetadata {
    id: "RPM102",
    name: "inequality-redundancy",
    description: "`X OP a && X OP b` where one constraint subsumes the other — drop the weaker side.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct InequalityRedundancy {
    diagnostics: Vec<Diagnostic>,
}

impl InequalityRedundancy {
    pub fn new() -> Self {
        Self::default()
    }

    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for branch in &node.branches {
            let CondExpr::Parsed(ast) = &branch.expr else {
                continue;
            };
            let (redundant, _) = analyse(ast);
            if redundant {
                self.diagnostics.push(
                    Diagnostic::new(
                        &REDUNDANCY_METADATA,
                        Severity::Warn,
                        "redundant inequality: one constraint already implies another \
                         in the same `&&`-chain",
                        branch.data,
                    )
                    .with_suggestion(Suggestion::new(
                        "drop the weaker comparison and keep only the tightest bound",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

impl<'ast> Visit<'ast> for InequalityRedundancy {
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

impl Lint for InequalityRedundancy {
    fn metadata(&self) -> &'static LintMetadata {
        &REDUNDANCY_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM103 inequality-contradiction
// =====================================================================

pub static CONTRADICTION_METADATA: LintMetadata = LintMetadata {
    id: "RPM103",
    name: "inequality-contradiction",
    description: "`&&`-chain has incompatible inequalities — the whole guard is always false.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct InequalityContradiction {
    diagnostics: Vec<Diagnostic>,
}

impl InequalityContradiction {
    pub fn new() -> Self {
        Self::default()
    }

    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for branch in &node.branches {
            let CondExpr::Parsed(ast) = &branch.expr else {
                continue;
            };
            let (_, contradicts_any) = analyse(ast);
            if contradicts_any {
                self.diagnostics.push(Diagnostic::new(
                    &CONTRADICTION_METADATA,
                    Severity::Warn,
                    "inequality contradiction: the `&&`-chain has incompatible bounds — \
                     the guard can never be true",
                    branch.data,
                ));
            }
        }
    }
}

impl<'ast> Visit<'ast> for InequalityContradiction {
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

impl Lint for InequalityContradiction {
    fn metadata(&self) -> &'static LintMetadata {
        &CONTRADICTION_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run<L: Lint>(src: &str, mut lint: L) -> Vec<Diagnostic> {
        let outcome = parse(src);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    // ---- RPM102 inequality-redundancy ----

    #[test]
    fn rpm102_flags_two_ge_same_lhs() {
        // `0%{?...}` parses as Raw (arithmetic-style concat isn't in
        // the ExprAst grammar); use a pure macro lhs to stay on the
        // Parsed path.
        let src = "Name: x\n%if %{?rhel} >= 8 && %{?rhel} >= 6\nLicense: MIT\n%endif\n";
        let diags = run(src, InequalityRedundancy::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM102");
    }

    #[test]
    fn rpm102_flags_two_le_same_lhs() {
        // Use `>` direction with a single variable to keep the
        // grammar inside ExprAst (it doesn't model arithmetic).
        let src = "Name: x\n%if X <= 5 && X <= 10\nLicense: MIT\n%endif\n";
        let diags = run(src, InequalityRedundancy::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn rpm102_silent_for_different_lhs() {
        let src = "Name: x\n%if X >= 8 && Y >= 6\nLicense: MIT\n%endif\n";
        assert!(run(src, InequalityRedundancy::new()).is_empty());
    }

    #[test]
    fn rpm102_silent_for_cross_direction() {
        // `X >= 5 && X <= 10` is a real range, not a redundancy.
        let src = "Name: x\n%if X >= 5 && X <= 10\nLicense: MIT\n%endif\n";
        assert!(run(src, InequalityRedundancy::new()).is_empty());
    }

    #[test]
    fn rpm102_silent_for_or_chain() {
        // `||` is outside the v1 scope — only top-level `&&`-chains.
        let src = "Name: x\n%if X >= 8 || X >= 6\nLicense: MIT\n%endif\n";
        assert!(run(src, InequalityRedundancy::new()).is_empty());
    }

    // ---- RPM103 inequality-contradiction ----

    #[test]
    fn rpm103_flags_empty_intersection() {
        let src = "Name: x\n%if X >= 10 && X < 5\nLicense: MIT\n%endif\n";
        let diags = run(src, InequalityContradiction::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM103");
    }

    #[test]
    fn rpm103_flags_boundary_contradiction() {
        // `X > 5 && X < 6` — only integer 5 would satisfy `> 5`, but
        // `< 6` excludes 6, so satisfiable. Whereas `X > 6 && X < 6`
        // is empty.
        let src = "Name: x\n%if X > 6 && X < 6\nLicense: MIT\n%endif\n";
        let diags = run(src, InequalityContradiction::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn rpm103_silent_for_satisfiable_range() {
        let src = "Name: x\n%if X >= 5 && X < 10\nLicense: MIT\n%endif\n";
        assert!(run(src, InequalityContradiction::new()).is_empty());
    }

    #[test]
    fn rpm103_silent_for_different_lhs() {
        let src = "Name: x\n%if X >= 10 && Y < 5\nLicense: MIT\n%endif\n";
        assert!(run(src, InequalityContradiction::new()).is_empty());
    }
}
