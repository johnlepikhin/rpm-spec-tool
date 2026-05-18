//! RPM431 `elif-history-simplify` — flag `%elif` expressions whose
//! head condition repeats a fact already guaranteed by earlier branches
//! of the same `%if`/`%elif` chain.
//!
//! In `%if A %elif !A && B`, the `!A` operand of the elif is implicit:
//! control only reaches `%elif` when every prior branch was false, so
//! `¬A` is already in scope. Writing `!A && B` instead of just `B`
//! is dead syntax that obscures the real test.
//!
//! Companion to RPM430 (`context-redundant-condition-part`), which
//! handles the same kind of redundancy for the *first* branch under
//! an outer path. RPM431 specialises to second-and-later branches and
//! the implicit `¬prior` history.

use rpm_spec::ast::{CondExpr, Conditional, FilesContent, PreambleContent, Span, SpecItem};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::path_cond::{
    MAX_PATH_STACK_DEPTH, PathConditions, branch_effective_dnf, compute_frame, cond_to_dnf,
    conjoin, else_effective_dnf, subtract_implied_literals, tautology_dnf, walk_files_body,
    walk_preamble_body, walk_top_body,
};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM431",
    name: "elif-history-simplify",
    description: "`%elif` expression repeats a fact already implied by prior branches' \
                  negations or by the enclosing path — drop the redundant operand.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// `%elif` expression repeats a fact already implied by prior branches' negations or by the enclosing path — drop the redundant operand.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct ElifHistorySimplify {
    diagnostics: Vec<Diagnostic>,
    pc: PathConditions,
}

impl ElifHistorySimplify {
    pub fn new() -> Self {
        Self::default()
    }

    fn check_elif(&mut self, expr: &CondExpr<Span>, prior: &[&CondExpr<Span>], anchor: Span) {
        // Build the implicit history DNF: `¬prior_0 ∧ ¬prior_1 ∧ …`.
        let Some(history) = else_effective_dnf(prior, &mut self.pc.atoms) else {
            return;
        };
        // Combine with outer path (if any) — the elif body runs under
        // the conjunction of both. Either side can imply a literal.
        let combined = match self.pc.current() {
            Some(outer) => match conjoin(outer, &history) {
                Some(d) => d,
                None => return, // explosion or empty (taint).
            },
            None => history,
        };
        if combined.is_empty() {
            return;
        }
        let head = cond_to_dnf(expr, &mut self.pc.atoms, false);
        let Some(head) = head else {
            return;
        };
        if head.len() != 1 {
            return;
        }
        let cube = head.iter().next().unwrap().clone();
        if cube.is_empty() {
            return;
        }
        let atom_count = self.pc.atoms.len();
        let Some((kept, dropped)) = subtract_implied_literals(&cube, &combined, atom_count) else {
            return;
        };
        if dropped.is_empty() {
            return;
        }
        // Whole-branch implication is RPM114/RPM115's territory — don't
        // double-warn here.
        if kept.is_empty() {
            return;
        }
        self.diagnostics.push(
            Diagnostic::new(
                &METADATA,
                Severity::Warn,
                format!(
                    "`%elif` expression contains {n} operand(s) already implied by prior \
                     branches' negations or the enclosing path; drop them",
                    n = dropped.len(),
                ),
                anchor,
            )
            .with_suggestion(Suggestion::new(
                "remove the redundant operand(s) from the `%elif`",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

fn walk_conditional<B, FB>(
    rule: &mut ElifHistorySimplify,
    node: &Conditional<Span, B>,
    walk_body: FB,
) where
    FB: Fn(&mut ElifHistorySimplify, &[B]),
{
    if node.branches.is_empty() {
        return;
    }
    if rule.pc.stack.len() >= MAX_PATH_STACK_DEPTH {
        return;
    }
    let tainted_above = rule.pc.is_tainted();
    let mut prior: Vec<&CondExpr<Span>> = Vec::with_capacity(node.branches.len());
    for (idx, branch) in node.branches.iter().enumerate() {
        let eff = branch_effective_dnf(&branch.expr, &prior, &mut rule.pc.atoms);
        if !tainted_above && idx >= 1 {
            rule.check_elif(&branch.expr, &prior, branch.data);
        }
        let frame = compute_frame(rule.pc.current(), eff);
        rule.pc.push(frame);
        walk_body(rule, &branch.body);
        rule.pc.pop();
        prior.push(&branch.expr);
    }
    if let Some(els) = node.otherwise.as_deref() {
        let eff = else_effective_dnf(&prior, &mut rule.pc.atoms).or_else(|| Some(tautology_dnf()));
        let frame = compute_frame(rule.pc.current(), eff);
        rule.pc.push(frame);
        walk_body(rule, els);
        rule.pc.pop();
    }
}

impl<'ast> Visit<'ast> for ElifHistorySimplify {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        walk_conditional(self, node, walk_top_body);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        walk_conditional(self, node, walk_preamble_body);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        walk_conditional(self, node, walk_files_body);
    }
}

impl Lint for ElifHistorySimplify {
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
        run_lint::<ElifHistorySimplify>(src)
    }

    #[test]
    fn flags_negated_prior_in_elif() {
        // `%if A %elif !A && B` — `!A` is implied by the prior branch's
        // negation, so the elif simplifies to `B`.
        let src = "Name: x\n\
%if A\n\
%global a 1\n\
%elif !A && B\n\
%global b 1\n\
%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM431");
    }

    #[test]
    fn flags_double_history_in_third_branch() {
        // `%if A %elif B %elif !A && !B && C` — both `!A` and `!B` are
        // implied by the elif history.
        let src = "Name: x\n\
%if A\n\
%global a 1\n\
%elif B\n\
%global b 1\n\
%elif !A && !B && C\n\
%global c 1\n\
%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn silent_for_non_redundant_elif() {
        let src = "Name: x\n\
%if A\n\
%global a 1\n\
%elif B && C\n\
%global b 1\n\
%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_first_branch() {
        // First branch has no prior — RPM430 covers it, not RPM431.
        let src = "Name: x\n\
%if A && B\n\
%global a 1\n\
%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_whole_elif_unsat() {
        // `%if A %elif A && B` — second branch is dead (`A` and `¬A`
        // are both required). RPM115 owns that; we don't double-warn.
        let src = "Name: x\n\
%if A\n\
%global a 1\n\
%elif A && B\n\
%global b 1\n\
%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_combined_outer_and_history() {
        // Outer `%if X`, inner chain `%if A %elif !A && X && B` — `!A`
        // from history AND `X` from outer are both implied.
        let src = "Name: x\n\
%if X\n\
%if A\n\
%global a 1\n\
%elif !A && X && B\n\
%global b 1\n\
%endif\n\
%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }
}
