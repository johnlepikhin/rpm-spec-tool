//! RPM115 `dead-elif-after-parent` — Phase 8b.
//!
//! Same machinery as RPM113 but anchors on `%elif` branches
//! (index ≥ 1). Separate id so users can silence the two cases
//! independently — a dead first `%if` and a dead `%elif` in a chain
//! are different smells in code review.

use rpm_spec::ast::{Conditional, FilesContent, PreambleContent, Span, SpecItem};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::boolean_dnf::Dnf;
use crate::rules::path_cond::{
    BranchAnalyser, PathConditions, analyse_conditional, conjoin, is_unsat,
};
use crate::visit::Visit;

pub static DEAD_ELIF_METADATA: LintMetadata = LintMetadata {
    id: "RPM115",
    name: "dead-elif-after-parent",
    description: "`%elif` branch is unsatisfiable under the ancestor path-condition combined \
         with negations of preceding sibling branches; its body is dead.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct DeadElif {
    diagnostics: Vec<Diagnostic>,
    pc: PathConditions,
}

impl DeadElif {
    pub fn new() -> Self {
        Self::default()
    }

    fn maybe_emit(&mut self, eff: &Option<Dnf>, anchor: Span) {
        let Some(eff) = eff else { return };
        let combined = match self.pc.current() {
            Some(path) => match conjoin(path, eff) {
                Some(d) => d,
                None => return,
            },
            None => eff.clone(),
        };
        if is_unsat(&combined) {
            self.diagnostics.push(
                Diagnostic::new(
                    &DEAD_ELIF_METADATA,
                    Severity::Warn,
                    "`%elif` branch cannot be satisfied under the path-condition \
                     and preceding sibling negations",
                    anchor,
                )
                .with_suggestion(Suggestion::new(
                    "remove the dead `%elif` clause or rework the chain",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

impl BranchAnalyser for DeadElif {
    fn pc(&mut self) -> &mut PathConditions {
        &mut self.pc
    }

    fn on_branch(&mut self, idx: usize, eff: &Option<Dnf>, anchor: Span) {
        if idx >= 1 {
            self.maybe_emit(eff, anchor);
        }
    }
}

fn walk_top_body(slf: &mut DeadElif, body: &[SpecItem<Span>]) {
    for item in body {
        slf.visit_item(item);
    }
}
fn walk_preamble_body(slf: &mut DeadElif, body: &[PreambleContent<Span>]) {
    for c in body {
        slf.visit_preamble_content(c);
    }
}
fn walk_files_body(slf: &mut DeadElif, body: &[FilesContent<Span>]) {
    for c in body {
        slf.visit_files_content(c);
    }
}

impl<'ast> Visit<'ast> for DeadElif {
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

impl Lint for DeadElif {
    fn metadata(&self) -> &'static LintMetadata {
        &DEAD_ELIF_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = DeadElif::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn rpm115_flags_elif_repeating_prior_branch() {
        // `%if A %elif A` — second branch_effective = A ∧ ¬A = ⊥.
        let src = "Name: x\n%if A\n%global foo bar\n%elif A\n%global baz qux\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM115");
    }

    #[test]
    fn rpm115_silent_for_first_branch_dead() {
        // `%if 0` is RPM072 / RPM113 territory, not RPM115.
        let src = "Name: x\n%if 0\n%global foo bar\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn rpm115_silent_for_satisfiable_elif() {
        let src = "Name: x\n%if A\n%global foo bar\n%elif B\n%global baz qux\n%endif\n";
        assert!(run(src).is_empty());
    }
}
