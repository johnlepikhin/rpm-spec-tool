//! RPM104 `string-set-redundancy` — `X == "a" || X == "a"` repeats
//! the same string in an `||`-chain — drop the duplicate.

use rpm_spec::ast::{CondExpr, Conditional, FilesContent, PreambleContent, Span, SpecItem};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::exprs_equiv;
use crate::visit::{self, Visit};

pub static STRING_SET_METADATA: LintMetadata = LintMetadata {
    id: "RPM104",
    name: "string-set-redundancy",
    description: "`X == \"a\" || X == \"a\"` repeats the same string in an `||`-chain — drop the duplicate.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct StringSetRedundancy {
    diagnostics: Vec<Diagnostic>,
}

impl StringSetRedundancy {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Walk the top-level `||`-chain of `ast` and accumulate operands.
fn flatten_or_chain<'a, T>(
    ast: &'a rpm_spec::ast::ExprAst<T>,
    out: &mut Vec<&'a rpm_spec::ast::ExprAst<T>>,
) {
    use rpm_spec::ast::{BinOp, ExprAst};
    match ast.peel_parens() {
        ExprAst::Binary {
            kind: BinOp::LogOr,
            lhs,
            rhs,
            ..
        } => {
            flatten_or_chain(lhs, out);
            flatten_or_chain(rhs, out);
        }
        other => out.push(other),
    }
}

/// `Some((lhs, string_value))` when `ast` is `LHS == "literal"`.
/// Any other shape (different op, non-string rhs) returns `None`.
fn extract_string_eq<T>(
    ast: &rpm_spec::ast::ExprAst<T>,
) -> Option<(&rpm_spec::ast::ExprAst<T>, &str)> {
    use rpm_spec::ast::{BinOp, ExprAst};
    if let ExprAst::Binary {
        kind: BinOp::Eq,
        lhs,
        rhs,
        ..
    } = ast.peel_parens()
        && let ExprAst::String { value, .. } = rhs.peel_parens()
    {
        return Some((lhs, value));
    }
    None
}

impl StringSetRedundancy {
    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for branch in &node.branches {
            let CondExpr::Parsed(ast) = &branch.expr else {
                continue;
            };
            let mut operands = Vec::new();
            flatten_or_chain(ast, &mut operands);
            if operands.len() < 2 {
                continue;
            }
            let pairs: Vec<_> = operands
                .iter()
                .filter_map(|o| extract_string_eq(o))
                .collect();
            // Look for any pair (i, j) with same lhs and same string.
            let mut found_dup = false;
            for i in 0..pairs.len() {
                for j in (i + 1)..pairs.len() {
                    let (lhs1, s1) = pairs[i];
                    let (lhs2, s2) = pairs[j];
                    if s1 == s2 && exprs_equiv(lhs1, lhs2) {
                        found_dup = true;
                        break;
                    }
                }
                if found_dup {
                    break;
                }
            }
            if found_dup {
                self.diagnostics.push(
                    Diagnostic::new(
                        &STRING_SET_METADATA,
                        Severity::Warn,
                        "duplicate `X == \"...\"` operand in `||`-chain — drop the repeat",
                        branch.data,
                    )
                    .with_suggestion(Suggestion::new(
                        "remove the repeated equality comparison",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

impl<'ast> Visit<'ast> for StringSetRedundancy {
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

impl Lint for StringSetRedundancy {
    fn metadata(&self) -> &'static LintMetadata {
        &STRING_SET_METADATA
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
    fn rpm104_flags_repeated_string_in_or() {
        let src = "Name: x\n%if %{?_vendor} == \"a\" || %{?_vendor} == \"b\" || %{?_vendor} == \"a\"\nLicense: MIT\n%endif\n";
        let diags = run(src, StringSetRedundancy::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM104");
    }

    #[test]
    fn rpm104_silent_for_unique_strings() {
        let src =
            "Name: x\n%if %{?_vendor} == \"a\" || %{?_vendor} == \"b\"\nLicense: MIT\n%endif\n";
        assert!(run(src, StringSetRedundancy::new()).is_empty());
    }

    #[test]
    fn rpm104_silent_for_different_lhs() {
        // Same literal `"a"` but compared against different macros — not a dupe.
        let src = "Name: x\n%if %{?x} == \"a\" || %{?y} == \"a\"\nLicense: MIT\n%endif\n";
        assert!(run(src, StringSetRedundancy::new()).is_empty());
    }
}
