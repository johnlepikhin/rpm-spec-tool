//! Conditional-expression helpers.
//!
//! Used by Phase 6 / Phase 7 rules that reason about the `CondExpr`
//! and `ExprAst` shapes the parser emits for `%if` / boolean
//! sub-expressions. Most helpers are conservative: any expression
//! containing a macro reference declines to resolve, because we can't
//! pin down what a macro expands to at lint time.

use rpm_spec::ast::{CondExpr, Text};

/// `true` when `expr` is a literal `1` / `true` — evaluates to true
/// unconditionally. Returns `false` for any expression containing
/// macros (we can't statically resolve those).
///
/// Handles both [`CondExpr::Raw`] (legacy raw-text path) and
/// [`CondExpr::Parsed`] (structured AST path). Both paths agree on
/// what counts as a constant truth.
pub(crate) fn is_constant_true_condition<T>(expr: &CondExpr<T>) -> bool {
    match expr {
        CondExpr::Parsed(ast) => match ast.as_ref().peel_parens() {
            rpm_spec::ast::ExprAst::Integer { value, .. } => *value != 0,
            rpm_spec::ast::ExprAst::String { value, .. } => !value.is_empty(),
            rpm_spec::ast::ExprAst::Identifier { name, .. } => name == "true",
            _ => false,
        },
        CondExpr::Raw(text) => {
            let Some(lit) = text.literal_str() else {
                return false;
            };
            matches!(lit.trim(), "1" | "true")
        }
        _ => false,
    }
}

/// `true` when `expr` is a literal `0` / `false` / empty — always
/// evaluates to false. RPM's expression language treats the empty
/// string as false, so `%if` with an empty body counts.
pub(crate) fn is_constant_false_condition<T>(expr: &CondExpr<T>) -> bool {
    match expr {
        CondExpr::Parsed(ast) => match ast.as_ref().peel_parens() {
            rpm_spec::ast::ExprAst::Integer { value, .. } => *value == 0,
            rpm_spec::ast::ExprAst::String { value, .. } => value.is_empty(),
            rpm_spec::ast::ExprAst::Identifier { name, .. } => name == "false",
            _ => false,
        },
        CondExpr::Raw(text) => {
            let Some(lit) = text.literal_str() else {
                return false;
            };
            matches!(lit.trim(), "0" | "false" | "")
        }
        _ => false,
    }
}

/// Conservative structural equality for two `CondExpr` values.
///
/// Returns `true` only when both expressions are statically
/// resolvable to the same text. Any expression that contains a `%`
/// (macro reference) causes a `false` return — the parser stores
/// the RPM expression language as raw text, so we can't tell what a
/// macro will expand to at build time, which is outside the lint's
/// scope.
///
/// `ArchList` arms must contain the same set of literal architecture
/// tokens (order-insensitive).
pub(crate) fn cond_expr_resolvably_eq<T, U>(a: &CondExpr<T>, b: &CondExpr<U>) -> bool {
    match (a, b) {
        (CondExpr::Raw(t1), CondExpr::Raw(t2)) => match (t1.literal_str(), t2.literal_str()) {
            (Some(s1), Some(s2)) => {
                let trimmed1 = s1.trim();
                let trimmed2 = s2.trim();
                // Conservative bail-out: any `%` likely starts a macro
                // reference (or `%%` literal). The parser doesn't
                // tokenise the expression grammar, so we can't tell
                // them apart — refuse to claim equivalence.
                if trimmed1.contains('%') || trimmed2.contains('%') {
                    return false;
                }
                trimmed1 == trimmed2
            }
            _ => false,
        },
        (CondExpr::ArchList(a1), CondExpr::ArchList(a2)) => {
            if a1.len() != a2.len() {
                return false;
            }
            let lit_set = |items: &[Text]| -> Option<Vec<String>> {
                let mut out = Vec::with_capacity(items.len());
                for t in items {
                    let s = t.literal_str()?.trim();
                    if s.contains('%') {
                        return None;
                    }
                    out.push(s.to_owned());
                }
                Some(out)
            };
            let (Some(mut s1), Some(mut s2)) = (lit_set(a1), lit_set(a2)) else {
                return false;
            };
            s1.sort();
            s2.sort();
            s1 == s2
        }
        // Parsed-vs-Parsed: structural equality of the AST (after
        // peeling parens) gives us a precise comparison without
        // re-stringifying. Any macro reference inside (whether
        // structurally identical or not) triggers a conservative
        // bail-out — we can't resolve `%{name}` at lint time.
        (CondExpr::Parsed(a1), CondExpr::Parsed(b1)) => {
            !contains_macro_ast(a1) && !contains_macro_ast(b1) && exprs_equiv(a1, b1)
        }
        // `Raw` vs `ArchList` (and any future variant) — different
        // shape; can't be equal. `CondExpr` is `#[non_exhaustive]`,
        // so the wildcard is required.
        _ => false,
    }
}

/// Structural equality for two [`ExprAst`] trees, ignoring `data`
/// (spans). Paren wrappers are peeled before comparison, so
/// `(X) && (Y)` compares equal to `X && Y`.
///
/// Used by [`cond_expr_resolvably_eq`] and Phase 7-series rules that
/// need to ask whether two sub-expressions denote the same value
/// (RPM086 idempotent, RPM088 self-comparison, RPM102 inequality
/// grouping, RPM104 string-set, RPM101 absorption).
pub(crate) fn exprs_equiv<T, U>(
    a: &rpm_spec::ast::ExprAst<T>,
    b: &rpm_spec::ast::ExprAst<U>,
) -> bool {
    use rpm_spec::ast::ExprAst;
    match (a.peel_parens(), b.peel_parens()) {
        (ExprAst::Integer { value: v1, .. }, ExprAst::Integer { value: v2, .. }) => v1 == v2,
        (ExprAst::String { value: v1, .. }, ExprAst::String { value: v2, .. }) => v1 == v2,
        (ExprAst::Identifier { name: n1, .. }, ExprAst::Identifier { name: n2, .. }) => n1 == n2,
        (ExprAst::Macro { text: m1, .. }, ExprAst::Macro { text: m2, .. }) => m1 == m2,
        (ExprAst::Not { inner: i1, .. }, ExprAst::Not { inner: i2, .. }) => exprs_equiv(i1, i2),
        (
            ExprAst::Binary {
                kind: k1,
                lhs: l1,
                rhs: r1,
                ..
            },
            ExprAst::Binary {
                kind: k2,
                lhs: l2,
                rhs: r2,
                ..
            },
        ) => k1 == k2 && exprs_equiv(l1, l2) && exprs_equiv(r1, r2),
        _ => false,
    }
}

/// `true` when the AST contains any macro reference — used by
/// [`cond_expr_resolvably_eq`] to bail out conservatively.
pub(crate) fn contains_macro_ast<T>(ast: &rpm_spec::ast::ExprAst<T>) -> bool {
    use rpm_spec::ast::ExprAst;
    match ast {
        ExprAst::Integer { .. } | ExprAst::String { .. } | ExprAst::Identifier { .. } => false,
        ExprAst::Macro { .. } => true,
        ExprAst::Paren { inner, .. } | ExprAst::Not { inner, .. } => contains_macro_ast(inner),
        ExprAst::Binary { lhs, rhs, .. } => contains_macro_ast(lhs) || contains_macro_ast(rhs),
        // `ExprAst` is `#[non_exhaustive]` from the upstream crate; a
        // future variant is treated conservatively as "contains
        // macros".
        _ => true,
    }
}

/// Flatten a tree of `||` ([`BinOp::LogOr`]) at the AST root, peeling
/// any `Paren` wrappers along the way. The recursion stops at the
/// first non-`||` operator, so a single non-`||` expression returns
/// `vec![&ast]`.
///
/// Generic over `S` (the AST's span phantom-data) so the same helper
/// is reused by rules that work on `ExprAst<Span>` and any future
/// non-`Span` parameterisation.
pub(crate) fn flatten_or<S>(ast: &rpm_spec::ast::ExprAst<S>) -> Vec<&rpm_spec::ast::ExprAst<S>> {
    use rpm_spec::ast::{BinOp, ExprAst};
    let mut out = Vec::new();
    fn rec<'a, S>(ast: &'a ExprAst<S>, out: &mut Vec<&'a ExprAst<S>>) {
        match ast.peel_parens() {
            ExprAst::Binary {
                kind: BinOp::LogOr,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;
    use rpm_spec::ast::Span;

    fn p(src: &str) -> rpm_spec::ast::SpecFile<Span> {
        parse(src).spec
    }

    // ---- flatten_or ----

    #[test]
    fn flatten_or_collapses_nested_or() {
        // Parse `%if (a||b)||c` and check that flatten_or returns three
        // operands. We piggy-back on the existing `parse` helper so we
        // don't have to hand-build an `ExprAst` tree.
        let spec = p("Name: x\n%if (a||b)||c\nLicense: MIT\n%endif\n");
        // Walk to the first conditional, find its branch expr's Parsed AST.
        let mut found: Option<(usize, usize)> = None;
        for item in &spec.items {
            if let rpm_spec::ast::SpecItem::Conditional(c) = item {
                if let CondExpr::Parsed(ast) = &c.branches[0].expr {
                    let parts = flatten_or(ast.as_ref());
                    found = Some((parts.len(), parts.len()));
                    assert_eq!(parts.len(), 3, "expected 3 disjuncts: {parts:?}");
                }
            }
        }
        assert!(found.is_some(), "no %if seen");
    }
}
