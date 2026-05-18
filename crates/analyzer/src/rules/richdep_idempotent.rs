//! RPM591 `richdep-idempotent` — flag rich/boolean dependency
//! expressions where an `and`/`or` group repeats the same operand.
//!
//! `(foo and foo)` is structurally noisy: RPM evaluates the duplicate
//! exactly like the singleton. The recommendation is to drop the
//! repeated operand. Matches at any nesting level: a duplicate inside
//! `(a and (b or c or c))` still fires.
//!
//! Operands are compared via [`crate::rules::util::dep_expr_canonical_eq`]
//! — structural equality with macro-aware bail-out. Atoms that differ
//! only in whitespace are equal; atoms whose name expands a macro are
//! NOT compared (we can't resolve the macro at lint time).
//!
//! `with` / `without` are intentionally NOT flagged: their operands are
//! positionally meaningful (subpackage/atom binding), so a "duplicate"
//! is not the same kind of code smell.

use rpm_spec::ast::{BoolDep, DepExpr, Span, SpecFile, TagValue};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{collect_top_level_preamble, dep_expr_canonical_eq};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM591",
    name: "richdep-idempotent",
    description: "Rich dependency expression repeats an operand inside an `and` / `or` group \
                  (`(foo and foo)` / `(foo or foo)`); drop the duplicate.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// Rich dependency expression repeats an operand inside an `and` / `or` group (`(foo and foo)` / `(foo or foo)`); drop the duplicate.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct RichdepIdempotent {
    diagnostics: Vec<Diagnostic>,
}

impl RichdepIdempotent {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for RichdepIdempotent {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for item in collect_top_level_preamble(spec) {
            let TagValue::Dep(expr) = &item.value else {
                continue;
            };
            if has_idempotent_group(expr) {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "rich dependency expression contains a duplicate operand inside an \
                         `and` / `or` group — drop the repeat",
                        item.data,
                    )
                    .with_suggestion(Suggestion::new(
                        "remove the repeated operand",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

fn has_idempotent_group(expr: &DepExpr) -> bool {
    match expr {
        DepExpr::Atom(_) => false,
        DepExpr::Rich(b) => bool_dep_has_idempotence(b),
        // `DepExpr` is `#[non_exhaustive]`; unknown variants are
        // conservatively not flagged.
        _ => false,
    }
}

fn bool_dep_has_idempotence(b: &BoolDep) -> bool {
    match b {
        BoolDep::And(xs) | BoolDep::Or(xs) => {
            // Look for any pair of structurally equal operands at this
            // level …
            for (i, x) in xs.iter().enumerate() {
                for y in &xs[i + 1..] {
                    if dep_expr_canonical_eq(x, y) {
                        return true;
                    }
                }
            }
            // … then recurse into each operand.
            xs.iter().any(has_idempotent_group)
        }
        // `with` and `without` are positional — no idempotence claim.
        BoolDep::With(xs) => xs.iter().any(has_idempotent_group),
        BoolDep::Without { left, right } => {
            has_idempotent_group(left) || has_idempotent_group(right)
        }
        BoolDep::If {
            cond,
            then,
            otherwise,
        }
        | BoolDep::Unless {
            cond,
            then,
            otherwise,
        } => {
            has_idempotent_group(cond)
                || has_idempotent_group(then)
                || otherwise.as_deref().is_some_and(has_idempotent_group)
        }
        // `BoolDep` is `#[non_exhaustive]`; future variants are
        // conservatively not flagged.
        _ => false,
    }
}

impl Lint for RichdepIdempotent {
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
        run_lint::<RichdepIdempotent>(src)
    }

    #[test]
    fn flags_and_with_duplicate_operand() {
        let src = "Name: x\nRequires: (foo and foo)\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM591");
    }

    #[test]
    fn flags_or_with_duplicate_operand() {
        let src = "Name: x\nRequires: (foo or foo)\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn flags_duplicate_in_three_term_and() {
        let src = "Name: x\nRequires: (foo and bar and foo)\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn flags_nested_duplicate() {
        // Inner `(b or b)` is flagged even though the outer `and` is
        // not idempotent.
        let src = "Name: x\nRequires: (a and (b or b))\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_on_distinct_operands() {
        let src = "Name: x\nRequires: (foo and bar)\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_on_plain_atom() {
        let src = "Name: x\nRequires: foo\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_operand_contains_macro() {
        // `dep_expr_canonical_eq` bails out on macros, so two textually
        // identical macro atoms are NOT treated as duplicates.
        let src = "Name: x\nRequires: (%{thing} and %{thing})\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_on_with_operand_duplicate() {
        // `with` is positional — even structurally equal operands are
        // not flagged.
        let src = "Name: x\nRequires: (foo with foo)\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn fires_in_build_requires_too() {
        let src = "Name: x\nBuildRequires: (alpha or alpha)\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }
}
