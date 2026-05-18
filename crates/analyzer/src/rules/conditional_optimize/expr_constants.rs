//! Expression-level lints (RPM085, RPM087).
//!
//! Both inspect the literal text of `%if`/`%elif` expressions. The
//! parser stores the expression as a single literal string when no
//! macro segmentation is needed — `0%{?rhel}` ends up as one
//! `Literal("0%{?rhel}")` — so a `%` in the string means there's a
//! macro reference *somewhere* and we bail out conservatively.

use rpm_spec::ast::{
    CondBranch, CondExpr, Conditional, FilesContent, PreambleContent, Span, SpecItem,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

/// View of an `%if` expression in one of the two parser-produced
/// forms. Rules that scan operators / operands at literal-text level
/// (RPM085, RPM087) work on `Raw`; structural rules use `Parsed`.
enum BranchExprView<'a, T> {
    Raw(&'a str),
    Parsed(&'a rpm_spec::ast::ExprAst<T>),
}

fn each_branch_expr<B>(
    node: &Conditional<Span, B>,
) -> impl Iterator<Item = (&CondBranch<Span, B>, BranchExprView<'_, Span>)> {
    node.branches.iter().filter_map(|b| match &b.expr {
        CondExpr::Raw(t) => t.literal_str().map(|s| (b, BranchExprView::Raw(s))),
        CondExpr::Parsed(ast) => Some((b, BranchExprView::Parsed(ast.as_ref()))),
        _ => None,
    })
}

// ---- RPM085 constant-tautology-in-expr ----

pub static CONSTANT_TAUTOLOGY_METADATA: LintMetadata = LintMetadata {
    id: "RPM085",
    name: "constant-tautology-in-expr",
    description: "Expression contains a constant operand (`|| 1`, `&& 0`, …) that fixes the result.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct ConstantTautologyInExpr {
    diagnostics: Vec<Diagnostic>,
}

impl ConstantTautologyInExpr {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Detect the simple constant-tautology patterns at top-level of the
/// expression. Returns a short label for the matched pattern so the
/// diagnostic can quote the offending fragment. Conservative: bails
/// on any `%` (macro presence) because the parser doesn't tokenise
/// the expression grammar.
fn detect_tautology(expr: &str) -> Option<&'static str> {
    let trimmed = expr.trim();
    if trimmed.contains('%') {
        return None;
    }
    // Normalise: `true` → `1`, `false` → `0`, drop whitespace. We
    // never rewrite the source, only the buffer we scan.
    let norm: String = trimmed
        .replace("true", "1")
        .replace("false", "0")
        .replace(char::is_whitespace, "");
    let bytes = norm.as_bytes();
    // Pattern "||1": reject if a digit follows the `1` (else `||10`
    // would match). Before the `||` anything is fine.
    if let Some(idx) = norm.find("||1") {
        let after = bytes.get(idx + 3).copied();
        if !matches!(after, Some(c) if c.is_ascii_digit()) {
            return Some("always-true (`|| 1`)");
        }
    }
    // Pattern "1||": reject if a digit precedes the `1`.
    if let Some(idx) = norm.find("1||") {
        let before = if idx == 0 {
            None
        } else {
            bytes.get(idx - 1).copied()
        };
        if !matches!(before, Some(c) if c.is_ascii_digit()) {
            return Some("always-true (`1 ||`)");
        }
    }
    // Pattern "&&0": reject if a digit follows the `0`.
    if let Some(idx) = norm.find("&&0") {
        let after = bytes.get(idx + 3).copied();
        if !matches!(after, Some(c) if c.is_ascii_digit()) {
            return Some("always-false (`&& 0`)");
        }
    }
    // Pattern "0&&": reject if a digit precedes the `0`.
    if let Some(idx) = norm.find("0&&") {
        let before = if idx == 0 {
            None
        } else {
            bytes.get(idx - 1).copied()
        };
        if !matches!(before, Some(c) if c.is_ascii_digit()) {
            return Some("always-false (`0 &&`)");
        }
    }
    None
}

impl ConstantTautologyInExpr {
    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for (branch, view) in each_branch_expr(node) {
            let label = match view {
                BranchExprView::Raw(s) => detect_tautology(s),
                BranchExprView::Parsed(ast) => detect_tautology_ast(ast),
            };
            if let Some(label) = label {
                self.diagnostics.push(
                    Diagnostic::new(
                        &CONSTANT_TAUTOLOGY_METADATA,
                        Severity::Warn,
                        format!("condition is {label}; simplify or drop the guard"),
                        branch.data,
                    )
                    .with_suggestion(Suggestion::new(
                        "drop the constant operand and re-check the rest of the expression",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

/// AST-based tautology detection mirroring [`detect_tautology`] but
/// walking the structured tree. Catches `Binary(LogOr, c, _)` where
/// `c` is constant-true (or symmetrically on the other side) and
/// `Binary(LogAnd, c, _)` where `c` is constant-false.
fn detect_tautology_ast<T>(ast: &rpm_spec::ast::ExprAst<T>) -> Option<&'static str> {
    use rpm_spec::ast::{BinOp, ExprAst};
    match ast.peel_parens() {
        ExprAst::Binary {
            kind: BinOp::LogOr,
            lhs,
            rhs,
            ..
        } => {
            if is_const_true_ast(lhs) || is_const_true_ast(rhs) {
                return Some("always-true (`|| 1`)");
            }
            // Recurse into children to catch nested tautologies.
            detect_tautology_ast(lhs).or_else(|| detect_tautology_ast(rhs))
        }
        ExprAst::Binary {
            kind: BinOp::LogAnd,
            lhs,
            rhs,
            ..
        } => {
            if is_const_false_ast(lhs) || is_const_false_ast(rhs) {
                return Some("always-false (`&& 0`)");
            }
            detect_tautology_ast(lhs).or_else(|| detect_tautology_ast(rhs))
        }
        ExprAst::Not { inner, .. } => detect_tautology_ast(inner),
        _ => None,
    }
}

fn is_const_true_ast<T>(ast: &rpm_spec::ast::ExprAst<T>) -> bool {
    use rpm_spec::ast::ExprAst;
    match ast.peel_parens() {
        ExprAst::Integer { value, .. } => *value != 0,
        ExprAst::String { value, .. } => !value.is_empty(),
        ExprAst::Identifier { name, .. } => name == "true",
        _ => false,
    }
}

fn is_const_false_ast<T>(ast: &rpm_spec::ast::ExprAst<T>) -> bool {
    use rpm_spec::ast::ExprAst;
    match ast.peel_parens() {
        ExprAst::Integer { value, .. } => *value == 0,
        ExprAst::String { value, .. } => value.is_empty(),
        ExprAst::Identifier { name, .. } => name == "false",
        _ => false,
    }
}

impl<'ast> Visit<'ast> for ConstantTautologyInExpr {
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

impl Lint for ConstantTautologyInExpr {
    fn metadata(&self) -> &'static LintMetadata {
        &CONSTANT_TAUTOLOGY_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// ---- RPM087 double-negation-in-expr ----

pub static DOUBLE_NEGATION_METADATA: LintMetadata = LintMetadata {
    id: "RPM087",
    name: "double-negation-in-expr",
    description: "Double negation (`!!`) in `%if` expression — drop it.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct DoubleNegationInExpr {
    diagnostics: Vec<Diagnostic>,
}

impl DoubleNegationInExpr {
    pub fn new() -> Self {
        Self::default()
    }
}

/// `true` if `expr` contains a `!!` token (two consecutive `!`s) in
/// its literal form. We don't try to handle `! !` with a space —
/// that's a different shape and would need real tokenisation.
fn has_double_negation(expr: &str) -> bool {
    expr.contains("!!")
}

/// AST-based double-negation: any `Not { inner: Not { .. } }` subtree
/// counts.
fn has_double_negation_ast<T>(ast: &rpm_spec::ast::ExprAst<T>) -> bool {
    use rpm_spec::ast::ExprAst;
    match ast {
        ExprAst::Not { inner, .. } => {
            matches!(inner.as_ref().peel_parens(), ExprAst::Not { .. })
                || has_double_negation_ast(inner)
        }
        ExprAst::Paren { inner, .. } => has_double_negation_ast(inner),
        ExprAst::Binary { lhs, rhs, .. } => {
            has_double_negation_ast(lhs) || has_double_negation_ast(rhs)
        }
        _ => false,
    }
}

impl DoubleNegationInExpr {
    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for (branch, view) in each_branch_expr(node) {
            let hit = match view {
                BranchExprView::Raw(s) => has_double_negation(s),
                BranchExprView::Parsed(ast) => has_double_negation_ast(ast),
            };
            if hit {
                self.diagnostics.push(
                    Diagnostic::new(
                        &DOUBLE_NEGATION_METADATA,
                        Severity::Warn,
                        "double negation `!!` is redundant — drop it",
                        branch.data,
                    )
                    .with_suggestion(Suggestion::new(
                        "remove the two `!` characters",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

impl<'ast> Visit<'ast> for DoubleNegationInExpr {
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

impl Lint for DoubleNegationInExpr {
    fn metadata(&self) -> &'static LintMetadata {
        &DOUBLE_NEGATION_METADATA
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

    // ---- RPM085 constant-tautology-in-expr ----

    #[test]
    fn rpm085_flags_or_one() {
        let src = "Name: x\n%if 0 || 1\nVersion: 1\n%endif\n";
        let diags = run(src, ConstantTautologyInExpr::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("always-true"));
    }

    #[test]
    fn rpm085_flags_and_zero() {
        let src = "Name: x\n%if 1 && 0\nVersion: 1\n%endif\n";
        let diags = run(src, ConstantTautologyInExpr::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("always-false"));
    }

    #[test]
    fn rpm085_flags_true_or() {
        let src = "Name: x\n%if true || 0\nVersion: 1\n%endif\n";
        let diags = run(src, ConstantTautologyInExpr::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn rpm085_silent_for_normal_or() {
        let src = "Name: x\n%if 0 || 0\nVersion: 1\n%endif\n";
        // `0 || 0` is always false but RPM072 already catches constant
        // conditions; RPM085 should not also fire on it (no `|| 1` or
        // `&& 0` pattern).
        //
        // Wait — `&& 0` matches `0 || 0`? No, `||` not `&&`.
        // `0 || 0` -> normalised "0||0" -> looking for "||1" or "1||" -> none.
        // -> looking for "&&0" or "0&&" -> "0&&" not present (we have "0||0").
        // OK, silent.
        assert!(run(src, ConstantTautologyInExpr::new()).is_empty());
    }

    #[test]
    fn rpm085_silent_for_macro_expression() {
        let src = "Name: x\n%if 0%{?rhel} || 1\nVersion: 1\n%endif\n";
        // Bails on `%` — conservative.
        assert!(run(src, ConstantTautologyInExpr::new()).is_empty());
    }

    #[test]
    fn rpm085_silent_for_non_constant_operands() {
        // Both sides are non-constant identifiers; nothing to flag.
        // (The earlier raw-text detector worried about substring
        // confusion like `|| 10` vs `|| 1`; with the AST path that's
        // a non-issue — `10` is correctly identified as the truthy
        // integer it is.)
        let src = "Name: x\n%if X || Y\nVersion: 1\n%endif\n";
        assert!(run(src, ConstantTautologyInExpr::new()).is_empty());
    }

    // ---- RPM087 double-negation-in-expr ----

    #[test]
    fn rpm087_flags_double_bang() {
        let src = "Name: x\n%if !!X\nVersion: 1\n%endif\n";
        let diags = run(src, DoubleNegationInExpr::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM087");
    }

    #[test]
    fn rpm087_flags_double_bang_with_macro() {
        // `!!` is itself non-macro text; works even if X is a macro.
        let src = "Name: x\n%if !!0%{?rhel}\nVersion: 1\n%endif\n";
        let diags = run(src, DoubleNegationInExpr::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn rpm087_silent_for_single_negation() {
        let src = "Name: x\n%if !X\nVersion: 1\n%endif\n";
        assert!(run(src, DoubleNegationInExpr::new()).is_empty());
    }

    #[test]
    fn rpm087_silent_for_not_equal() {
        // `!=` contains `!` but no `!!`.
        let src = "Name: x\n%if X != 1\nVersion: 1\n%endif\n";
        assert!(run(src, DoubleNegationInExpr::new()).is_empty());
    }
}
