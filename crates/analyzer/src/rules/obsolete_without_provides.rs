//! RPM034 `obsolete-without-provides` — when a package obsoletes
//! another, it should also `Provides:` the obsoleted name so that
//! upgrading users keep getting the functionality. Without a matching
//! Provides, dependent packages break on upgrade.
//!
//! Subpackage-aware: each `%package` block is checked against its own
//! Obsoletes and Provides sets. Skips:
//! - obsoletes whose name contains a macro (can't compare literally),
//! - obsoletes that look like file paths (`Obsoletes: /usr/bin/...`).

use std::collections::HashSet;

use rpm_spec::ast::{Span, SpecFile, Tag};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{collect_dep_atoms_in_items, iter_packages};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM034",
    name: "obsolete-without-provides",
    description: "Each Obsoletes entry should be matched by a Provides of the same name to keep upgrades smooth.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct ObsoleteWithoutProvides {
    diagnostics: Vec<Diagnostic>,
}

impl ObsoleteWithoutProvides {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for ObsoleteWithoutProvides {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for pkg in iter_packages(spec) {
            let obsoletes =
                collect_dep_atoms_in_items(pkg.items(), |t| matches!(t, Tag::Obsoletes));
            if obsoletes.is_empty() {
                continue;
            }
            let provides_names: HashSet<&str> =
                collect_dep_atoms_in_items(pkg.items(), |t| matches!(t, Tag::Provides))
                    .iter()
                    .filter_map(|a| a.name.literal_str())
                    .collect();

            for atom in obsoletes {
                let Some(obs_name) = atom.name.literal_str() else {
                    continue; // macroized name, can't compare
                };
                if obs_name.starts_with('/') {
                    continue; // file-path obsoletes are uncommon and legitimate
                }
                if !provides_names.contains(obs_name) {
                    self.diagnostics.push(Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        format!(
                            "`Obsoletes: {obs_name}` has no matching `Provides:` — \
                             upgraders of {obs_name} may lose the functionality"
                        ),
                        pkg.header_span(),
                    ));
                }
            }
        }
    }
}

impl Lint for ObsoleteWithoutProvides {
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
        let mut lint = ObsoleteWithoutProvides::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_obsolete_without_provides() {
        let diags = run("Name: hello\nObsoletes: old-hello\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM034");
    }

    #[test]
    fn silent_when_provides_matches() {
        assert!(run("Name: hello\nObsoletes: old-hello\nProvides: old-hello\n").is_empty());
    }

    #[test]
    fn skips_path_obsoletes() {
        // file-path obsoletes are rare and legitimate; we conservatively
        // don't flag them
        assert!(run("Name: hello\nObsoletes: /usr/bin/old\n").is_empty());
    }

    #[test]
    fn skips_macroized_obsoletes() {
        // Name with a macro can't be matched literally; conservatively skip.
        assert!(run("Name: hello\nObsoletes: %{macro_name}\n").is_empty());
    }

    #[test]
    fn flags_subpackage_obsolete_without_provides() {
        // Regression lock for the subpackage-aware code path: the
        // subpackage `foo` declares `Obsoletes: old-foo` with no
        // matching `Provides:` *inside the same subpackage* (provides
        // in main package don't count).
        let src = "Name: main\n\
%package -n foo\n\
Summary: standalone\n\
Obsoletes: old-foo\n\
%description -n foo\nbody\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "got {diags:?}");
        assert!(diags[0].message.contains("old-foo"));
    }
}
