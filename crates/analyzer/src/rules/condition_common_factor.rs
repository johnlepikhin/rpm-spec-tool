//! RPM432 `condition-common-factor` — flag `%if` expressions whose
//! DNF cubes share a common operand that could be factored out.
//!
//! `(A && B) || (A && C)` simplifies to `A && (B || C)`: the user
//! wrote `A` twice when once would do. The rule looks at the head
//! expression of every branch and reports when **every** cube of the
//! normalised DNF shares at least one literal.
//!
//! Distinct from RPM110 (`boolean-dnf-redundancy`), which fires when a
//! cube *subsumes* another; RPM432 fires when no subsumption applies
//! but there's still a shared factor.
//!
//! Default severity `Allow`: the factored form is not unambiguously
//! shorter for every text shape (cost model is a heuristic), so this
//! rule ships off by default and is opt-in for spec authors who like
//! the factored style.

use rpm_spec::ast::{CondExpr, Conditional, FilesContent, PreambleContent, Span, SpecItem};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::boolean_dnf::{AtomTable, Cube, Literal, simplify_subsumption};
use crate::rules::path_cond::cond_to_dnf;
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM432",
    name: "condition-common-factor",
    description: "`%if` expression's normalised DNF cubes share a common operand — factor it \
                  out (`(A && B) || (A && C)` → `A && (B || C)`).",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// `%if` expression's normalised DNF cubes share a common operand — factor it out (`(A && B) || (A && C)` → `A && (B || C)`).
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct ConditionCommonFactor {
    diagnostics: Vec<Diagnostic>,
}

impl ConditionCommonFactor {
    pub fn new() -> Self {
        Self::default()
    }

    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for branch in &node.branches {
            self.check_branch(&branch.expr, branch.data);
        }
    }

    fn check_branch(&mut self, expr: &CondExpr<Span>, anchor: Span) {
        let mut atoms = AtomTable::new();
        let Some(raw) = cond_to_dnf(expr, &mut atoms, false) else {
            return;
        };
        let (dnf, _) = simplify_subsumption(&raw);
        // Need at least two cubes for "common factor" to mean anything.
        if dnf.len() < 2 {
            return;
        }
        // Reject degenerate cubes (empty cube = always-true; RPM111 owns
        // that).
        if dnf.iter().any(|c| c.is_empty()) {
            return;
        }
        let common = common_literals(&dnf);
        if common.is_empty() {
            return;
        }
        // Each cube has at least one extra literal beyond the common
        // factor — otherwise some cube *is* the factor and would be
        // subsumed, which simplify_subsumption already handled.
        if dnf
            .iter()
            .any(|c| c.len() <= common.len() || !common.is_subset(c))
        {
            return;
        }
        self.diagnostics.push(
            Diagnostic::new(
                &METADATA,
                Severity::Warn,
                format!(
                    "`%if` expression has {n} cube(s) all sharing {k} common literal(s); \
                     factor the shared operand(s) out",
                    n = dnf.len(),
                    k = common.len(),
                ),
                anchor,
            )
            .with_suggestion(Suggestion::new(
                "rewrite as `factor && (rest_1 || rest_2 || …)`",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

/// Intersection of literals across every cube in `dnf`. Empty when at
/// least one cube has no overlap.
fn common_literals(dnf: &std::collections::BTreeSet<Cube>) -> std::collections::BTreeSet<Literal> {
    let mut iter = dnf.iter();
    let first = match iter.next() {
        Some(c) => c.clone(),
        None => return Default::default(),
    };
    let mut acc = first;
    for c in iter {
        acc.retain(|lit| c.contains(lit));
        if acc.is_empty() {
            return acc;
        }
    }
    acc
}

impl<'ast> Visit<'ast> for ConditionCommonFactor {
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

impl Lint for ConditionCommonFactor {
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
        run_lint::<ConditionCommonFactor>(src)
    }

    #[test]
    fn flags_two_cube_common_factor() {
        let src = "Name: x\n%if (A && B) || (A && C)\nLicense: MIT\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM432");
    }

    #[test]
    fn flags_three_cube_common_factor() {
        let src = "Name: x\n%if (A && B) || (A && C) || (A && D)\nLicense: MIT\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn silent_for_single_cube() {
        let src = "Name: x\n%if A && B && C\nLicense: MIT\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_no_common_factor() {
        let src = "Name: x\n%if (A && B) || (C && D)\nLicense: MIT\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_subsumption_already_collapsed() {
        // `(A && B) || (A && B && C)` simplifies via subsumption to
        // `A && B`. After subsumption, only one cube remains — RPM432
        // bows out (RPM110 already fires here).
        let src = "Name: x\n%if (A && B) || (A && B && C)\nLicense: MIT\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn fires_with_negated_common_factor() {
        let src = "Name: x\n%if (!A && B) || (!A && C)\nLicense: MIT\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn silent_when_one_cube_is_just_the_factor() {
        // `A || (A && B)` — first cube IS the factor, so subsumption
        // collapses it to `A` (handled by RPM101/RPM110).
        let src = "Name: x\n%if A || (A && B)\nLicense: MIT\n%endif\n";
        assert!(run(src).is_empty());
    }
}
