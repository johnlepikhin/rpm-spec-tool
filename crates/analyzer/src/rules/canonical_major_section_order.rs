//! RPM573 `canonical-major-section-order` — flag major sections that
//! appear out of canonical order (`%description` → `%prep` → `%build`
//! → `%install` → `%check` → `%files` → scriptlets → `%changelog`).
//!
//! Reviewers expect the canonical layout; deviations slow the eye
//! down on every review pass.

use rpm_spec::ast::{BuildScriptKind, Section, Span, SpecFile, SpecItem};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM573",
    name: "canonical-major-section-order",
    description: "Major sections appear out of canonical order \
                  (description → prep → build → install → check → files → scriptlets → changelog).",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// Major sections appear out of canonical order (description → prep → build → install → check → files → scriptlets → changelog).
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct CanonicalMajorSectionOrder {
    diagnostics: Vec<Diagnostic>,
}

impl CanonicalMajorSectionOrder {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for CanonicalMajorSectionOrder {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let mut max_seen = 0u8;
        for item in &spec.items {
            let SpecItem::Section(boxed) = item else {
                continue;
            };
            let Some((w, span)) = section_weight(boxed.as_ref()) else {
                continue;
            };
            if w < max_seen {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "section appears out of canonical order (description → prep → build → \
                         install → check → files → scriptlets → changelog)",
                        span,
                    )
                    .with_suggestion(Suggestion::new(
                        "move this section to its canonical position",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            } else {
                max_seen = w;
            }
        }
    }
}

fn section_weight(s: &Section<Span>) -> Option<(u8, Span)> {
    match s {
        Section::Description { data, .. } => Some((1, *data)),
        Section::Package { data, .. } => Some((1, *data)),
        Section::BuildScript { kind, data, .. } => {
            // BuildScriptKind is #[non_exhaustive]; canonical order is
            // description → prep → build → install → check → files → scriptlets → changelog.
            // %clean is deprecated and omitted from canonical ordering — its
            // presence is reported separately by RPM021. Unknown future
            // variants skip ordering analysis (None) rather than being
            // silently assigned a guessed weight.
            let w = match kind {
                BuildScriptKind::Prep => 2,
                BuildScriptKind::Build => 3,
                BuildScriptKind::Install => 4,
                BuildScriptKind::Check => 5,
                BuildScriptKind::Clean => return None,
                _ => return None,
            };
            Some((w, *data))
        }
        Section::Files { data, .. } => Some((6, *data)),
        Section::Scriptlet(scr) => Some((7, scr.data)),
        Section::Trigger(tr) => Some((7, tr.data)),
        Section::FileTrigger(_) => None,
        Section::Verify { data, .. } => Some((7, *data)),
        Section::Sepolicy { data, .. } => Some((7, *data)),
        Section::Changelog { data, .. } => Some((8, *data)),
        Section::SourceList { data, .. } => Some((1, *data)),
        Section::PatchList { data, .. } => Some((1, *data)),
        _ => None,
    }
}

impl Lint for CanonicalMajorSectionOrder {
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
        run_lint::<CanonicalMajorSectionOrder>(src)
    }

    #[test]
    fn flags_files_before_install() {
        let src = "Name: x\n%files\n%{_bindir}/foo\n%install\nmkdir -p target\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM573");
    }

    #[test]
    fn flags_check_after_files() {
        let src = "Name: x\n%install\necho i\n%files\n%{_bindir}/foo\n%check\nmake test\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_canonical_order() {
        let src = "Name: x\n\
%description\nx\n\
%prep\necho p\n\
%build\necho b\n\
%install\necho i\n\
%check\necho c\n\
%files\n%{_bindir}/foo\n\
%changelog\n* Mon Jan 01 2024 me 1-1\n- init\n";
        assert!(run(src).is_empty());
    }
}
