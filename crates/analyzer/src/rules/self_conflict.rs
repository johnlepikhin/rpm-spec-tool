//! RPM040 `self-conflict` — symmetric to RPM033, but on `Conflicts:`.
//!
//! `Conflicts: %{name}` prevents the package from coexisting with
//! itself, which physically blocks installation. Almost always a typo
//! where the author meant a different package name.
//!
//! Subpackage-aware: each `%package` block is checked against its own
//! resolved name.

use rpm_spec::ast::{Span, SpecFile, Tag};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{collect_dep_atoms_in_items, iter_packages};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM040",
    name: "self-conflict",
    description: "A package declares a Conflicts entry naming itself, which blocks installation.",
    default_severity: Severity::Deny,
    category: LintCategory::Correctness,
};

/// A package declares a Conflicts entry naming itself, which blocks installation.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct SelfConflict {
    diagnostics: Vec<Diagnostic>,
}

impl SelfConflict {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for SelfConflict {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for pkg in iter_packages(spec) {
            let Some(name) = pkg.name() else {
                continue;
            };
            let conflicts =
                collect_dep_atoms_in_items(pkg.items(), |t| matches!(t, Tag::Conflicts));
            for atom in conflicts {
                if atom.name.literal_str() == Some(name) {
                    self.diagnostics.push(
                        Diagnostic::new(
                            &METADATA,
                            Severity::Deny,
                            format!("package `{name}` conflicts with itself"),
                            pkg.header_span(),
                        )
                        .with_label(pkg.header_span(), "package declared here"),
                    );
                }
            }
        }
    }
}

impl Lint for SelfConflict {
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
        run_lint::<SelfConflict>(src)
    }

    #[test]
    fn flags_main_self_conflict() {
        let diags = run("Name: hello\nConflicts: hello\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM040");
    }

    #[test]
    fn silent_when_conflicting_with_other_package() {
        assert!(run("Name: hello\nConflicts: old-other\n").is_empty());
    }

    #[test]
    fn flags_subpackage_self_conflict() {
        let diags = run("Name: main\n\
%package -n foo\n\
Conflicts: foo\n\
%description -n foo\nbody\n");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("foo"));
    }

    #[test]
    fn flags_subpackage_relative_self_conflict() {
        // Regression lock for relative subpackage name resolution:
        // `%package devel` against `Name: main` resolves to `main-devel`.
        let src = "Name: main\n\
%package devel\n\
Summary: dev files\n\
Conflicts: main-devel\n\
%description devel\nbody\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "got {diags:?}");
        assert!(diags[0].message.contains("main-devel"));
    }
}
