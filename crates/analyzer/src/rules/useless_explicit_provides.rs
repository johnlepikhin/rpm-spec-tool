//! RPM035 `useless-explicit-provides` — `Provides: <self>` without a
//! version is redundant: rpm auto-generates `Provides: name = epoch:version-release`
//! for every package. An unversioned self-provides only shadows the
//! generated one with weaker information.
//!
//! Versioned `Provides: %{name} = %{version}` is fine — it's an
//! explicit override, not redundancy.
//!
//! Auto-fix: drop the whole `Provides:` line. Subpackage-aware.

use rpm_spec::ast::{PreambleItem, Span, SpecFile, Tag, TagValue};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{drop_span, iter_packages};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM035",
    name: "useless-explicit-provides",
    description: "Explicit `Provides:` of the package's own name is redundant with rpm's auto-provides.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct UselessExplicitProvides {
    diagnostics: Vec<Diagnostic>,
}

impl UselessExplicitProvides {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for UselessExplicitProvides {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for pkg in iter_packages(spec) {
            let Some(name) = pkg.name() else {
                continue;
            };
            for item in pkg.items() {
                if !matches!(item.tag, Tag::Provides) {
                    continue;
                }
                // Only flag when the entire `Provides:` line is a
                // single unversioned atom matching the package name.
                // Mixed lines like `Provides: foo, bar(virtual)` keep
                // their other atoms intact; we conservatively skip them.
                if is_unversioned_provides_of(item, name) {
                    self.diagnostics.push(
                        Diagnostic::new(
                            &METADATA,
                            Severity::Warn,
                            format!("`Provides: {name}` is implied by rpm auto-provides"),
                            item.data,
                        )
                        .with_suggestion(Suggestion::new(
                            "remove the redundant Provides line",
                            vec![drop_span(item.data)],
                            Applicability::MachineApplicable,
                        )),
                    );
                }
            }
        }
    }
}

fn is_unversioned_provides_of(item: &PreambleItem<Span>, name: &str) -> bool {
    let TagValue::Dep(rpm_spec::ast::DepExpr::Atom(atom)) = &item.value else {
        return false;
    };
    atom.constraint.is_none() && atom.name.literal_str() == Some(name)
}

impl Lint for UselessExplicitProvides {
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
        let mut lint = UselessExplicitProvides::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_unversioned_self_provides() {
        let diags = run("Name: hello\nProvides: hello\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM035");
        assert!(!diags[0].suggestions.is_empty());
    }

    #[test]
    fn silent_for_versioned_self_provides() {
        // Versioned form is an intentional override, not noise.
        assert!(run("Name: hello\nProvides: hello = 1.0\n").is_empty());
    }

    #[test]
    fn silent_for_other_package_provides() {
        assert!(run("Name: hello\nProvides: virtual-thing\n").is_empty());
    }

    #[test]
    fn flags_subpackage_useless_provides() {
        let diags = run("Name: main\n\
%package -n foo\n\
Provides: foo\n\
%description -n foo\nbody\n");
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn flags_first_atom_of_mixed_provides_line() {
        // Regression lock: the rpm-spec parser desugars
        // `Provides: hello, virt` into *two* `PreambleItem`s, one per
        // atom. Our rule checks each independently — so the `hello`
        // atom is correctly flagged on its own line, and the auto-fix
        // span covers only that item.
        let src = "Name: hello\nProvides: hello, virt\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("hello"));
    }
}
