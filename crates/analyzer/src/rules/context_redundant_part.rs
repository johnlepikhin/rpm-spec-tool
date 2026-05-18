//! RPM430 `context-redundant-condition-part` — flag `%if` conditions
//! that contain a literal already implied by the enclosing path.
//!
//! Example: under `%if A`, an inner `%if A && B` is equivalent to
//! `%if B` because `A` is guaranteed by the outer condition. Unlike
//! RPM075 (`redundant-nested-condition`) and RPM114
//! (`always-true-branch-under-parent`) which detect when the *whole*
//! inner test is implied, this rule fires when only *part* of a
//! conjunction is implied — the head expression itself still narrows
//! control flow, but some operands are dead weight.
//!
//! Restricted to single-cube (pure conjunction) head expressions; a
//! disjunctive head like `(A && B) || (C && D)` is not analysed here.

use rpm_spec::ast::{CondExpr, Conditional, FilesContent, PreambleContent, Span, SpecItem};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::path_cond::{
    MAX_PATH_STACK_DEPTH, PathConditions, branch_effective_dnf, compute_frame, cond_to_dnf,
    else_effective_dnf, subtract_implied_literals, walk_files_body, walk_preamble_body,
    walk_top_body,
};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM430",
    name: "context-redundant-condition-part",
    description: "`%if` expression contains a conjunct already implied by the enclosing \
                  path — drop the redundant operand.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// `%if` expression contains a conjunct already implied by the enclosing path — drop the redundant operand.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct ContextRedundantPart {
    diagnostics: Vec<Diagnostic>,
    pc: PathConditions,
}

impl ContextRedundantPart {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run the redundancy check for one branch's head expression.
    ///
    /// `expr` is the head condition as written in source (without prior
    /// elif negations). Comparing only this against the outer path
    /// avoids reporting elif's auto-added negations as redundant — that
    /// is RPM431's job.
    fn check_head(&mut self, expr: &CondExpr<Span>, anchor: Span) {
        let Some(path) = self.pc.current() else {
            return;
        };
        if path.is_empty() {
            return;
        }
        let path = path.clone();
        let head = cond_to_dnf(expr, &mut self.pc.atoms, false);
        let Some(head) = head else {
            return;
        };
        // Single-cube head only: pure conjunction (e.g. `A && B && C`).
        if head.len() != 1 {
            return;
        }
        let cube = head.iter().next().unwrap().clone();
        if cube.is_empty() {
            return;
        }
        let atom_count = self.pc.atoms.len();
        let Some((kept, dropped)) = subtract_implied_literals(&cube, &path, atom_count) else {
            return;
        };
        if dropped.is_empty() {
            return;
        }
        // Only fire when at least one literal remains — if every literal
        // is implied, the entire branch is "always true", which RPM114
        // already flags. Avoid the noisy double-warn.
        if kept.is_empty() {
            return;
        }
        self.diagnostics.push(
            Diagnostic::new(
                &METADATA,
                Severity::Warn,
                format!(
                    "`%if` expression contains {n} operand(s) already implied by the \
                     enclosing path; drop them",
                    n = dropped.len(),
                ),
                anchor,
            )
            .with_suggestion(Suggestion::new(
                "remove the redundant operand(s) from the inner `%if`",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

/// Walk a conditional with full path-condition tracking AND access to
/// each branch's head expression. Equivalent to [`analyse_conditional`]
/// in the path-cond engine but yields the head expression to the rule
/// instead of an opaque `eff` DNF, which lets us reason about the
/// user's source text vs. compiler-inserted elif negations.
fn walk_conditional<B, FB>(
    rule: &mut ContextRedundantPart,
    node: &Conditional<Span, B>,
    walk_body: FB,
) where
    FB: Fn(&mut ContextRedundantPart, &[B]),
{
    if node.branches.is_empty() {
        return;
    }
    if rule.pc.stack.len() >= MAX_PATH_STACK_DEPTH {
        return;
    }
    let tainted_above = rule.pc.is_tainted();
    let mut prior: Vec<&CondExpr<Span>> = Vec::with_capacity(node.branches.len());
    for branch in &node.branches {
        let eff = branch_effective_dnf(&branch.expr, &prior, &mut rule.pc.atoms);
        if !tainted_above {
            rule.check_head(&branch.expr, branch.data);
        }
        let frame = compute_frame(rule.pc.current(), eff);
        rule.pc.push(frame);
        walk_body(rule, &branch.body);
        rule.pc.pop();
        prior.push(&branch.expr);
    }
    if let Some(els) = node.otherwise.as_deref() {
        let eff = else_effective_dnf(&prior, &mut rule.pc.atoms);
        let frame = compute_frame(rule.pc.current(), eff);
        rule.pc.push(frame);
        walk_body(rule, els);
        rule.pc.pop();
    }
}

impl<'ast> Visit<'ast> for ContextRedundantPart {
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

impl Lint for ContextRedundantPart {
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
        run_lint::<ContextRedundantPart>(src)
    }

    #[test]
    fn flags_redundant_conjunct_in_inner_if() {
        // Outer `%if A`, inner `%if A && B` → A is redundant in inner.
        let src = "Name: x\n%if A\n%if A && B\n%global foo bar\n%endif\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM430");
    }

    #[test]
    fn flags_redundant_conjunct_under_two_outer_levels() {
        // Outer `%if A`, middle `%if B`, inner `%if A && C` — `A` from
        // outer is implied; inner's `C` survives.
        let src = "Name: x\n%if A\n%if B\n%if A && C\n%global foo bar\n%endif\n%endif\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn silent_when_inner_fully_implied() {
        // `%if A` outer, inner `%if A` — every literal is implied; the
        // entire branch is always-true. RPM114 owns that case; this
        // rule deliberately stays silent to avoid a double-warn.
        let src = "Name: x\n%if A\n%if A\n%global foo bar\n%endif\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_inner_has_no_redundancy() {
        let src = "Name: x\n%if A\n%if B && C\n%global foo bar\n%endif\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_at_root_level() {
        // No enclosing path-condition; nothing to imply against.
        let src = "Name: x\n%if A && B\n%global foo bar\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_disjunctive_head() {
        // Inner `(A && B) || C` has two cubes; this rule's single-cube
        // restriction makes it skip — left to a future extension.
        let src = "Name: x\n%if A\n%if (A && B) || C\n%global foo bar\n%endif\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_with_negation_in_path() {
        // Outer `%if !A`, inner `%if !A && B` → !A is redundant.
        let src = "Name: x\n%if !A\n%if !A && B\n%global foo bar\n%endif\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }
}
