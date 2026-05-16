//! RPM033 `self-obsoletion` — a package obsoleting itself is almost
//! always a packaging bug: `Obsoletes: foo` in the spec whose `Name`
//! is `foo` tells rpm to remove the package on install, which makes
//! upgrades impossible. Fedora's rpmlint refuses such specs outright.
//!
//! Subpackage-aware: each `%package` block is checked against its own
//! resolved name (`main-suffix` for `%package suffix`, the literal
//! argument for `%package -n name`).

use rpm_spec::ast::{Span, SpecFile, Tag};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{collect_dep_atoms_in_items, iter_packages};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM033",
    name: "self-obsoletion",
    description: "A package declares an Obsoletes entry naming itself, which prevents upgrades.",
    default_severity: Severity::Deny,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct SelfObsoletion {
    diagnostics: Vec<Diagnostic>,
}

impl SelfObsoletion {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for SelfObsoletion {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for pkg in iter_packages(spec) {
            let Some(name) = pkg.name() else {
                continue;
            };
            let obsoletes =
                collect_dep_atoms_in_items(pkg.items(), |t| matches!(t, Tag::Obsoletes));
            for atom in obsoletes {
                if atom.name.literal_str() == Some(name) {
                    self.diagnostics.push(
                        Diagnostic::new(
                            &METADATA,
                            Severity::Deny,
                            format!("package `{name}` obsoletes itself"),
                            pkg.header_span(),
                        )
                        .with_label(pkg.header_span(), "package declared here"),
                    );
                }
            }
        }
    }
}

impl Lint for SelfObsoletion {
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
        let mut lint = SelfObsoletion::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_main_self_obsoletion() {
        let diags = run("Name: hello\nObsoletes: hello\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM033");
        assert!(diags[0].message.contains("hello"));
    }

    #[test]
    fn silent_when_obsoleting_other_package() {
        let diags = run("Name: hello\nObsoletes: old-hello\n");
        assert!(diags.is_empty());
    }

    #[test]
    fn flags_subpackage_self_obsoletion_absolute() {
        // `%package -n foo` resolves to `foo`; `Obsoletes: foo` is a
        // self-obsoletion of the subpackage.
        let diags = run("Name: main\n\
%package -n foo\n\
Obsoletes: foo\n\
%description -n foo\nbody\n");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("foo"));
    }

    #[test]
    fn flags_subpackage_self_obsoletion_relative() {
        // `%package devel` resolves to `main-devel`.
        let diags = run("Name: main\n\
%package devel\n\
Obsoletes: main-devel\n\
%description devel\nbody\n");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("main-devel"));
    }
}
