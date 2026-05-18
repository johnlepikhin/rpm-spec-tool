//! RPM002 `empty-description` — `%description` body should not be empty or
//! whitespace-only. An empty description ships in the RPM header and confuses
//! users browsing repository metadata.

use rpm_spec::ast::{Section, Span, TextBody, TextSegment};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM002",
    name: "empty-description",
    description: "%description bodies should not be empty.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// %description bodies should not be empty.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct EmptyDescription {
    diagnostics: Vec<Diagnostic>,
}

impl EmptyDescription {
    pub fn new() -> Self {
        Self::default()
    }
}

fn body_is_empty(body: &TextBody) -> bool {
    body.lines.iter().all(|line| {
        line.segments.iter().all(|seg| match seg {
            TextSegment::Literal(s) => s.trim().is_empty(),
            TextSegment::Macro(_) => false,
            _ => true,
        })
    })
}

impl<'ast> Visit<'ast> for EmptyDescription {
    fn visit_section(&mut self, node: &'ast Section<Span>) {
        if let Section::Description { body, data, .. } = node
            && body_is_empty(body)
        {
            self.diagnostics.push(Diagnostic::new(
                &METADATA,
                Severity::Warn,
                "%description body is empty",
                *data,
            ));
        }
        visit::walk_section(self, node);
    }
}

impl Lint for EmptyDescription {
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
        run_lint::<EmptyDescription>(src)
    }

    #[test]
    fn flags_empty_description() {
        let diags = run("%description\n\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM002");
    }

    #[test]
    fn silent_when_description_has_text() {
        let diags = run("%description\nHello.\n");
        assert!(diags.is_empty(), "{diags:?}");
    }
}
