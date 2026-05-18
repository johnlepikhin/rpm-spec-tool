//! RPM114 `always-true-branch-under-parent` — Phase 8b.
//!
//! Fires when `path ⊨ branch_effective` — i.e. every assignment that
//! satisfies the ancestor path also satisfies the branch's effective
//! condition. The `%if` test is redundant: control always flows into
//! this branch, so the `%if` wrapper can be unfolded.
//!
//! Subsumes RPM075 (`redundant-nested-condition`) at the semantic
//! level. Both rules can fire on syntactic duplicates; users may
//! silence whichever feels noisier.
//!
//! Uses truth-table enumeration via [`path_implies`]; for >8 atoms we
//! conservatively return `None` and skip the diagnostic.

use rpm_spec::ast::{Conditional, FilesContent, PreambleContent, Span, SpecItem};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::boolean_dnf::Dnf;
use crate::rules::path_cond::{
    BranchAnalyser, PathConditions, analyse_conditional, path_implies, walk_files_body,
    walk_preamble_body, walk_top_body,
};
use crate::visit::Visit;

pub static ALWAYS_TRUE_METADATA: LintMetadata = LintMetadata {
    id: "RPM114",
    name: "always-true-branch-under-parent",
    description: "`%if` branch is implied by the enclosing path-condition; the test is \
         redundant and the body always runs.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// `%if` branch is implied by the enclosing path-condition; the test is redundant and the body always runs.
///
/// See [`ALWAYS_TRUE_METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct AlwaysTrueBranch {
    diagnostics: Vec<Diagnostic>,
    pc: PathConditions,
}

impl AlwaysTrueBranch {
    pub fn new() -> Self {
        Self::default()
    }

    fn maybe_emit(&mut self, eff: &Option<Dnf>, anchor: Span) {
        let Some(eff) = eff else { return };
        // Only meaningful when there IS a non-trivial path. At the
        // root, "always true" means RPM072 (`%if 1`); not our concern.
        let Some(path) = self.pc.current() else {
            return;
        };
        if path.is_empty() {
            return;
        }
        let atom_count = self.pc.atoms.len();
        if let Some(true) = path_implies(path, eff, atom_count) {
            self.diagnostics.push(
                Diagnostic::new(
                    &ALWAYS_TRUE_METADATA,
                    Severity::Warn,
                    "`%if` condition is implied by the enclosing path; the test is redundant",
                    anchor,
                )
                .with_suggestion(Suggestion::new(
                    "unfold the inner `%if` — its body always runs under the parent",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

impl BranchAnalyser for AlwaysTrueBranch {
    fn pc(&mut self) -> &mut PathConditions {
        &mut self.pc
    }

    fn on_branch(&mut self, _idx: usize, eff: &Option<Dnf>, anchor: Span) {
        self.maybe_emit(eff, anchor);
    }
}

impl<'ast> Visit<'ast> for AlwaysTrueBranch {
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

impl Lint for AlwaysTrueBranch {
    fn metadata(&self) -> &'static LintMetadata {
        &ALWAYS_TRUE_METADATA
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
        run_lint::<AlwaysTrueBranch>(src)
    }

    #[test]
    fn rpm114_flags_implied_inner_repeat() {
        // path = X; inner = X — implied → fires.
        let src = "Name: x\n%if X\n%if X\n%global foo bar\n%endif\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM114");
    }

    #[test]
    fn rpm114_flags_implied_inner_subset() {
        // path = X && Y; inner = X — implied.
        let src = "Name: x\n%if X && Y\n%if X\n%global foo bar\n%endif\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn rpm114_silent_for_independent_inner() {
        let src = "Name: x\n%if X\n%if Y\n%global foo bar\n%endif\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn rpm114_silent_at_root_level() {
        let src = "Name: x\n%if 1\n%global foo bar\n%endif\n";
        assert!(run(src).is_empty());
    }
}
