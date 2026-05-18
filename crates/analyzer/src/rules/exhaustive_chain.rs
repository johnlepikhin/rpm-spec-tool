//! RPM116 `mutex-branches-spell-out-else` — Phase 8b.
//!
//! Fires on `%if A %elif B [%elif C] %endif` chains **without**
//! `%else`, where the branches' effective conditions collectively
//! exhaust the path-condition space — i.e. the implicit else region
//! `path ∧ ¬A ∧ ¬B [...]` is UNSAT. The last `%elif` is then
//! semantically equivalent to `%else` and the chain is clearer
//! spelled that way.

use rpm_spec::ast::{CondExpr, Conditional, FilesContent, PreambleContent, Span, SpecItem};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::path_cond::{
    BranchAnalyser, PathConditions, analyse_conditional, conjoin, else_effective_dnf, is_unsat,
    walk_files_body, walk_preamble_body, walk_top_body,
};
use crate::visit::Visit;

pub static EXHAUSTIVE_CHAIN_METADATA: LintMetadata = LintMetadata {
    id: "RPM116",
    name: "mutex-branches-spell-out-else",
    description: "`%if`/`%elif` chain exhausts the path-condition space yet lacks an explicit \
         `%else`; rewriting the last `%elif` as `%else` makes the chain clearer.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Minimum number of branches (one `%if` + one `%elif`) required for
/// a chain to be a meaningful RPM116 target. With a single `%if` there
/// is nothing to "spell out as `%else`".
const MIN_CHAIN_FOR_ELSE_COLLAPSE: usize = 2;

/// `%if`/`%elif` chain exhausts the path-condition space yet lacks an explicit `%else`; rewriting the last `%elif` as `%else` makes the chain clearer.
///
/// See [`EXHAUSTIVE_CHAIN_METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct ExhaustiveChain {
    diagnostics: Vec<Diagnostic>,
    pc: PathConditions,
    /// Anchor of the last branch seen during the most recent
    /// `analyse_conditional` pass. Stashed so `on_post_chain` knows
    /// where to point the diagnostic.
    last_branch_anchor: Option<Span>,
}

impl ExhaustiveChain {
    pub fn new() -> Self {
        Self::default()
    }
}

impl BranchAnalyser for ExhaustiveChain {
    fn pc(&mut self) -> &mut PathConditions {
        &mut self.pc
    }

    fn on_branch(&mut self, _idx: usize, _eff: &Option<Dnf>, anchor: Span) {
        self.last_branch_anchor = Some(anchor);
    }

    fn on_post_chain(&mut self, _node_anchor: Span, has_else: bool, prior: &[&CondExpr<Span>]) {
        if has_else || prior.len() < MIN_CHAIN_FOR_ELSE_COLLAPSE {
            return;
        }
        let Some(else_eff) = else_effective_dnf(prior, &mut self.pc.atoms) else {
            return;
        };
        let combined = match self.pc.current() {
            Some(path) => match conjoin(path, &else_eff) {
                Some(d) => d,
                None => return,
            },
            None => else_eff,
        };
        if is_unsat(&combined) {
            let anchor = self
                .last_branch_anchor
                .expect("on_branch records every branch's anchor before on_post_chain");
            self.diagnostics.push(
                Diagnostic::new(
                    &EXHAUSTIVE_CHAIN_METADATA,
                    Severity::Warn,
                    "branches exhaust the path-condition space; the final `%elif` \
                     is equivalent to `%else`",
                    anchor,
                )
                .with_suggestion(Suggestion::new(
                    "replace the final `%elif EXPR` with `%else`",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

use crate::rules::boolean_dnf::Dnf;

impl<'ast> Visit<'ast> for ExhaustiveChain {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        analyse_conditional(self, node, walk_top_body);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        analyse_conditional(self, node, walk_preamble_body);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        analyse_conditional(self, node, walk_files_body);
    }
}

impl Lint for ExhaustiveChain {
    fn metadata(&self) -> &'static LintMetadata {
        &EXHAUSTIVE_CHAIN_METADATA
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
        run_lint::<ExhaustiveChain>(src)
    }

    #[test]
    fn rpm116_flags_chain_covers_space() {
        // %if A %elif !A %endif — implicit-else region is ¬A ∧ ¬¬A = ⊥.
        let src = "Name: x\n%if A\n%global foo bar\n%elif !A\n%global baz qux\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM116");
    }

    #[test]
    fn rpm116_silent_when_else_present() {
        let src = "Name: x\n%if A\n%global foo bar\n%elif !A\n%global baz qux\n%else\n%global w v\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn rpm116_silent_for_non_exhaustive() {
        let src = "Name: x\n%if A\n%global foo bar\n%elif B\n%global baz qux\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn rpm116_silent_for_single_if() {
        // No %elif chain to collapse.
        let src = "Name: x\n%if A\n%global foo bar\n%endif\n";
        assert!(run(src).is_empty());
    }
}
