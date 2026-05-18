//! RPM023 `duplicate-buildscript-section` — rpm silently keeps **only
//! one** body for each build-script kind (`%prep`, `%build`, `%install`,
//! `%check`, etc.). Declaring two `%build` sections is a copy-paste
//! mistake; the earlier body is dead code.
//!
//! Only top-level sections are counted. The rule is global (not
//! subpackage-aware) because build-script sections themselves are
//! global — rpm doesn't honour `%build` inside `%package`.

use rpm_spec::ast::{BuildScriptKind, Section, Span, SpecFile, SpecItem};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM023",
    name: "duplicate-buildscript-section",
    description: "Spec declares the same build-script section (%prep/%build/%install/...) more than once.",
    default_severity: Severity::Deny,
    category: LintCategory::Packaging,
};

/// Spec declares the same build-script section (%prep/%build/%install/...) more than once.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct DuplicateBuildscriptSection {
    diagnostics: Vec<Diagnostic>,
}

impl DuplicateBuildscriptSection {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for DuplicateBuildscriptSection {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        // `BuildScriptKind` has only 7 variants so a small linear-scan
        // Vec is both simpler and faster than a HashMap (and the AST
        // type doesn't derive Hash).
        let mut seen: Vec<(BuildScriptKind, Span)> = Vec::new();
        for item in &spec.items {
            let SpecItem::Section(boxed) = item else {
                continue;
            };
            let Section::BuildScript { kind, data, .. } = boxed.as_ref() else {
                continue;
            };
            if let Some(&(_, first)) = seen.iter().find(|(k, _)| k == kind) {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Deny,
                        format!(
                            "duplicate {} section; rpm honours only one body",
                            section_keyword(*kind)
                        ),
                        *data,
                    )
                    .with_label(first, "first declaration here"),
                );
            } else {
                seen.push((*kind, *data));
            }
        }
    }
}

fn section_keyword(kind: BuildScriptKind) -> &'static str {
    match kind {
        BuildScriptKind::Prep => "%prep",
        BuildScriptKind::Conf => "%conf",
        BuildScriptKind::Build => "%build",
        BuildScriptKind::Install => "%install",
        BuildScriptKind::Check => "%check",
        BuildScriptKind::Clean => "%clean",
        BuildScriptKind::GenerateBuildRequires => "%generate_buildrequires",
        _ => "%<unknown>",
    }
}

impl Lint for DuplicateBuildscriptSection {
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
        run_lint::<DuplicateBuildscriptSection>(src)
    }

    #[test]
    fn flags_two_build_sections() {
        let src = "Name: x\n%build\nmake\n%build\nmake clean\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM023");
        assert!(diags[0].message.contains("%build"));
        assert_eq!(diags[0].labels.len(), 1);
    }

    #[test]
    fn flags_two_prep_sections() {
        let src = "Name: x\n%prep\n%setup -q\n%prep\n%setup -q\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("%prep"));
    }

    #[test]
    fn silent_when_each_kind_appears_once() {
        let src = "Name: x\n%prep\n%build\n%install\n%check\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn distinct_kinds_do_not_collide() {
        // `%build` once, `%install` once — no duplicate.
        let src = "Name: x\n%build\nmake\n%install\nmake install\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_two_install_sections() {
        let src = "Name: x\n%install\ncp a b\n%install\ncp c d\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("%install"));
    }
}
