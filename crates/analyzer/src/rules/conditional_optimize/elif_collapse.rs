//! RPM083 `collapse-elif-into-else` — final `%elif` with a
//! constant-true expression is equivalent to `%else`.

use rpm_spec::ast::{CondKind, Conditional, FilesContent, PreambleContent, Span, SpecItem};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

pub static COLLAPSE_ELIF_METADATA: LintMetadata = LintMetadata {
    id: "RPM083",
    name: "collapse-elif-into-else",
    description: "Final `%elif` with a constant-true expression is equivalent to `%else`.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct CollapseElifIntoElse {
    diagnostics: Vec<Diagnostic>,
}

impl CollapseElifIntoElse {
    pub fn new() -> Self {
        Self::default()
    }

    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        // Need at least one `%elif` (i.e. branches.len() >= 2). And
        // no `%else` already — otherwise the final `%elif true` is
        // either redundant or covered by other rules.
        if node.branches.len() < 2 || node.otherwise.is_some() {
            return;
        }
        let Some(last) = node.branches.last() else {
            return;
        };
        if !matches!(last.kind, CondKind::Elif) {
            return;
        }
        if !crate::rules::util::is_constant_true_condition(&last.expr) {
            return;
        }
        self.diagnostics.push(
            Diagnostic::new(
                &COLLAPSE_ELIF_METADATA,
                Severity::Warn,
                "final `%elif` with constant-true condition can become `%else`",
                last.data,
            )
            .with_suggestion(Suggestion::new(
                "replace the `%elif <true>` keyword line with `%else`",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

impl<'ast> Visit<'ast> for CollapseElifIntoElse {
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

impl Lint for CollapseElifIntoElse {
    fn metadata(&self) -> &'static LintMetadata {
        &COLLAPSE_ELIF_METADATA
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
    fn rpm083_flags_final_elif_true() {
        let src = "Name: x\n%if 0\nLicense: MIT\n%elif 1\nLicense: GPL\n%endif\n";
        let diags = run(src, CollapseElifIntoElse::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM083");
    }

    #[test]
    fn rpm083_silent_when_already_has_else() {
        let src =
            "Name: x\n%if 0\nLicense: MIT\n%elif 1\nLicense: GPL\n%else\nLicense: BSD\n%endif\n";
        assert!(run(src, CollapseElifIntoElse::new()).is_empty());
    }

    #[test]
    fn rpm083_silent_when_elif_not_constant_true() {
        let src = "Name: x\n%if 0\nLicense: MIT\n%elif X\nLicense: GPL\n%endif\n";
        assert!(run(src, CollapseElifIntoElse::new()).is_empty());
    }
}
