//! RPM001 `missing-changelog` — every spec should carry a `%changelog`
//! section. Maintainers rely on the section as a tamper-evident audit log;
//! Fedora packaging guidelines treat its absence as a build defect.

use rpm_spec::ast::{Section, Span, SpecFile};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM001",
    name: "missing-changelog",
    description: "Every spec file should declare a %changelog section.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

#[derive(Debug, Default)]
pub struct MissingChangelog {
    has_changelog: bool,
    spec_span: Option<Span>,
    diagnostics: Vec<Diagnostic>,
}

impl MissingChangelog {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for MissingChangelog {
    fn visit_spec(&mut self, node: &'ast SpecFile<Span>) {
        self.has_changelog = false;
        self.spec_span = Some(node.data);
        visit::walk_spec(self, node);

        if !self.has_changelog
            && let Some(span) = self.spec_span
        {
            self.diagnostics.push(Diagnostic::new(
                &METADATA,
                Severity::Warn,
                "spec file has no %changelog section",
                span,
            ));
        }
    }

    fn visit_section(&mut self, node: &'ast Section<Span>) {
        if matches!(node, Section::Changelog { .. }) {
            self.has_changelog = true;
        }
        visit::walk_section(self, node);
    }
}

impl Lint for MissingChangelog {
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
        let mut lint = MissingChangelog::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_missing_changelog() {
        let diags = run("Name: hello\nVersion: 1\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM001");
    }

    #[test]
    fn silent_when_changelog_present() {
        let src = "Name: x\n%changelog\n* Mon Jan 1 2024 a <a@b> - 1-1\n- init\n";
        let diags = run(src);
        assert!(diags.is_empty(), "{diags:?}");
    }
}
