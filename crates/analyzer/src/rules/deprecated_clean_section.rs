//! RPM021 `deprecated-clean-section` — `%clean` is unnecessary in modern
//! rpm: the buildroot is cleaned automatically before each build. Fedora,
//! openSUSE and others explicitly say not to include one.
//!
//! Auto-fix: drop the whole section. The span on `Section::data` covers
//! the header (`%clean`) and the body up to (but not including) the next
//! section header or EOF.

use rpm_spec::ast::{BuildScriptKind, Section, Span};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::drop_span;
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM021",
    name: "deprecated-clean-section",
    description: "The %clean section is unnecessary; modern rpm cleans the buildroot automatically.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

#[derive(Debug, Default)]
pub struct DeprecatedCleanSection {
    diagnostics: Vec<Diagnostic>,
}

impl DeprecatedCleanSection {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for DeprecatedCleanSection {
    fn visit_section(&mut self, node: &'ast Section<Span>) {
        if let Section::BuildScript { kind: BuildScriptKind::Clean, data, .. } = node {
            let diag = Diagnostic::new(
                &METADATA,
                Severity::Warn,
                "%clean section is no longer needed",
                *data,
            )
            .with_suggestion(Suggestion::new(
                "remove the %clean section",
                vec![drop_span(*data)],
                Applicability::MachineApplicable,
            ));
            self.diagnostics.push(diag);
        }
        visit::walk_section(self, node);
    }
}

impl Lint for DeprecatedCleanSection {
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
    use crate::session::parse;

    fn run(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = DeprecatedCleanSection::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_clean_section() {
        let src = "Name: x\n%clean\nrm -rf %{buildroot}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM021");
        assert!(!diags[0].suggestions.is_empty());
    }

    #[test]
    fn silent_when_no_clean_section() {
        let src = "Name: x\n%build\nmake\n";
        assert!(run(src).is_empty());
    }
}
