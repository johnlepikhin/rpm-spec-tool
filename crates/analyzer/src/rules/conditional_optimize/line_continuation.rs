//! RPM094 `line-continuation-in-condition` — `%if` expression spans
//! multiple lines via `\` — RPM doesn't support continuation here.

use rpm_spec::ast::{CondExpr, Conditional, FilesContent, PreambleContent, Span, SpecItem};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

pub static LINE_CONT_METADATA: LintMetadata = LintMetadata {
    id: "RPM094",
    name: "line-continuation-in-condition",
    description: "`%if` expression spans multiple lines via `\\` — RPM doesn't support continuation here.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct LineContinuationInCondition {
    diagnostics: Vec<Diagnostic>,
}

impl LineContinuationInCondition {
    pub fn new() -> Self {
        Self::default()
    }

    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for branch in &node.branches {
            let CondExpr::Raw(text) = &branch.expr else {
                continue;
            };
            let Some(lit) = text.literal_str() else {
                continue;
            };
            // `logical_line` joins continuation lines with a literal
            // `\n` separator — a `\n` mid-expression is the signal
            // that the author tried to split `%if` across lines.
            if lit.contains('\n') {
                self.diagnostics.push(Diagnostic::new(
                    &LINE_CONT_METADATA,
                    Severity::Warn,
                    "`%if` expression continues onto another line — \
                     RPM does not honour `\\` continuation in conditions",
                    branch.data,
                ));
            }
        }
    }
}

impl<'ast> Visit<'ast> for LineContinuationInCondition {
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

impl Lint for LineContinuationInCondition {
    fn metadata(&self) -> &'static LintMetadata {
        &LINE_CONT_METADATA
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
    fn rpm094_flags_continuation() {
        // `%if A \<NL>  B` joins to a literal containing `\n`.
        // The parser falls back to Raw because the joined text isn't
        // valid expression grammar.
        let src = "Name: x\n%if A \\\n  B\nLicense: MIT\n%endif\n";
        let diags = run(src, LineContinuationInCondition::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM094");
    }

    #[test]
    fn rpm094_silent_for_normal_if() {
        let src = "Name: x\n%if 1\nLicense: MIT\n%endif\n";
        assert!(run(src, LineContinuationInCondition::new()).is_empty());
    }
}
