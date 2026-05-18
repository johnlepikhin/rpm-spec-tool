//! RPM022 `multiple-changelog-sections` — there should be exactly one
//! `%changelog` section. rpm processes only the first one but silently
//! accepts more, which is a recipe for losing entries.

use rpm_spec::ast::{Section, Span, SpecFile};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM022",
    name: "multiple-changelog-sections",
    description: "\
Spec file declares more than one top-level %changelog section. rpm \
processes only the first one and silently drops the rest. Note: \
%changelog blocks nested inside %if/%endif are ignored on purpose — \
they're rare and usually intentional cross-distro patterns.",
    default_severity: Severity::Deny,
    category: LintCategory::Packaging,
};

/// Spec file declares more than one top-level %changelog section. rpm processes only the first one and silently drops the rest. Note: %changelog blocks nested inside %if/%endif are ignored on purpose — they're rare and usually intentional cross-distro patterns.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct MultipleChangelog {
    diagnostics: Vec<Diagnostic>,
}

impl MultipleChangelog {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for MultipleChangelog {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let mut seen_first: Option<Span> = None;
        // We only look at top-level items: a `%changelog` inside a
        // conditional `%if` block is rare and intentional.
        for item in &spec.items {
            if let rpm_spec::ast::SpecItem::Section(section) = item
                && let Section::Changelog { data, .. } = section.as_ref()
            {
                match seen_first {
                    None => seen_first = Some(*data),
                    Some(first) => {
                        self.diagnostics.push(
                            Diagnostic::new(
                                &METADATA,
                                Severity::Deny,
                                "duplicate %changelog section",
                                *data,
                            )
                            .with_label(first, "first %changelog declared here"),
                        );
                    }
                }
            }
        }
    }
}

impl Lint for MultipleChangelog {
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
        run_lint::<MultipleChangelog>(src)
    }

    #[test]
    fn flags_two_changelog_sections() {
        let src = "Name: x\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- A\n\
%changelog\n* Tue Jan 02 2024 b <b@c> - 1-2\n- B\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM022");
    }

    #[test]
    fn silent_when_one_changelog() {
        let src = "Name: x\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- A\n";
        assert!(run(src).is_empty());
    }
}
