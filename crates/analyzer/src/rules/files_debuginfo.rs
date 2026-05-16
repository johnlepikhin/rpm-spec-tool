//! RPM371 `debuginfo-path-in-main-files` — main-package `%files` lists
//! a path under `/usr/lib/debug` or referencing `.build-id` / `.debug`
//! suffix.
//!
//! Those paths are owned by the auto-generated `*-debuginfo` and
//! `*-debugsource` subpackages that rpmbuild produces when
//! `%debug_package` is enabled. Manually packaging them into the main
//! `%files` causes file conflicts at install time (the debuginfo RPM
//! already claims them) and breaks the per-package debuginfo extraction
//! pipeline.
//!
//! The rule stays silent when the section belongs to a subpackage
//! whose name suggests debug ownership — `-debuginfo`, `-debugsource`.

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::files::{FilesClassifier, for_each_files_entry_with_subpkg, resolve_subpkg_name};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::package_name;
use crate::visit::Visit;
use rpm_spec_profile::Profile;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM371",
    name: "debuginfo-path-in-main-files",
    description: "A `%files` entry points at `/usr/lib/debug` or a `.build-id`/`.debug` path. \
                  Those are owned by the auto-generated `-debuginfo` subpackage; remove the \
                  manual entry to avoid install-time file conflicts.",
    default_severity: Severity::Deny,
    category: LintCategory::Packaging,
};

#[derive(Debug, Default)]
pub struct DebuginfoPathInMainFiles {
    diagnostics: Vec<Diagnostic>,
    profile: Profile,
}

impl DebuginfoPathInMainFiles {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for DebuginfoPathInMainFiles {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let classifier = FilesClassifier::new(&self.profile);
        let main = package_name(spec).map(str::to_owned);

        for_each_files_entry_with_subpkg(spec, |subpkg, entry| {
            let pkg = resolve_subpkg_name(main.as_deref(), subpkg)
                .or_else(|| main.clone())
                .unwrap_or_default();
            if is_debuginfo_package(&pkg) {
                return;
            }
            let cls = classifier.classify(entry);
            if !cls.kind_hints.under_debug {
                return;
            }
            let path = cls.resolved_path.as_deref().unwrap_or("");
            self.diagnostics.push(Diagnostic::new(
                &METADATA,
                Severity::Deny,
                format!(
                    "`{path}` belongs in the auto-generated debuginfo subpackage; remove it \
                     from package `{pkg}`"
                ),
                cls.span(),
            ));
        });
    }
}

fn is_debuginfo_package(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with("-debuginfo") || lower.ends_with("-debugsource") || lower.ends_with("-debug")
}

impl Lint for DebuginfoPathInMainFiles {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_profile(&mut self, profile: &Profile) {
        self.profile = profile.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = DebuginfoPathInMainFiles::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_usr_lib_debug_in_main() {
        let src = "Name: x\n%files\n/usr/lib/debug/usr/bin/x.debug\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM371");
        assert_eq!(diags[0].severity, Severity::Deny);
    }

    #[test]
    fn flags_build_id_path() {
        let src = "Name: x\n%files\n/usr/lib/debug/.build-id/ab/cdef.debug\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn silent_for_debuginfo_subpackage() {
        let src = "Name: x\n\
%package debuginfo\n\
Summary: dbg\n\
%description debuginfo\nbody\n\
%files debuginfo\n\
/usr/lib/debug/usr/bin/x.debug\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_regular_paths() {
        let src = "Name: x\n%files\n/usr/bin/x\n";
        assert!(run(src).is_empty());
    }
}
