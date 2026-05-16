//! RPM325 `pkgconfig-file-without-pkgconfig-br` — `%files` ships a
//! `.pc` file but `BuildRequires:` doesn't include `pkgconfig`.
//!
//! Packages that install a `.pc` are themselves *providers* of a
//! `pkgconfig(...)` capability, and modern rpm auto-generates that
//! provide via the `pkgconfig`-package's helper. If the build host
//! doesn't have `pkgconfig` installed, the generator won't run and
//! downstream `-devel` consumers can't find the capability.
//!
//! The rule is gated to Fedora-, openSUSE-, ALT- and Mageia-style
//! families — every distro in this set packages the `.pc`-to-provides
//! generator inside the `pkgconfig` BR. On Generic / unknown profiles
//! the convention isn't established, so the rule stays silent.

use rpm_spec::ast::{Span, SpecFile, Tag};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::files::{FilesClassifier, for_each_files_entry};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::collect_top_level_dep_names;
use crate::visit::Visit;
use rpm_spec_profile::{Family, Profile};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM325",
    name: "pkgconfig-file-without-pkgconfig-br",
    description: "`%files` ships a `.pc` file but `BuildRequires:` lacks `pkgconfig`. Without \
                  the BR, rpm's `pkgconfig(...)` provides generator does not run; downstream \
                  `-devel` consumers can't find the capability.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

#[derive(Debug, Default)]
pub struct PkgconfigFileWithoutPkgconfigBr {
    diagnostics: Vec<Diagnostic>,
    profile: Profile,
    enabled: bool,
}

impl PkgconfigFileWithoutPkgconfigBr {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Families that ship the `.pc`-to-`pkgconfig(...)`-provides generator
/// inside their `pkgconfig` BR. On any other family the rule stays
/// silent — the convention may not apply.
fn family_applies(profile: &Profile) -> bool {
    matches!(
        profile.identity.family,
        Some(Family::Fedora | Family::Rhel | Family::Opensuse | Family::Mageia | Family::Alt)
    )
}

impl<'ast> Visit<'ast> for PkgconfigFileWithoutPkgconfigBr {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        if !self.enabled {
            return;
        }
        let classifier = FilesClassifier::new(&self.profile);
        // Find the first .pc entry; if none, the rule has no signal.
        let mut first_pc_span: Option<Span> = None;
        for_each_files_entry(spec, |entry| {
            if first_pc_span.is_some() {
                return;
            }
            let cls = classifier.classify(entry);
            if cls.kind_hints.is_pkgconfig {
                first_pc_span = Some(cls.span());
            }
        });
        let Some(span) = first_pc_span else {
            return;
        };
        let brs = collect_top_level_dep_names(spec, |t| matches!(t, Tag::BuildRequires));
        if brs.contains("pkgconfig") {
            return;
        }
        self.diagnostics.push(Diagnostic::new(
            &METADATA,
            Severity::Warn,
            "package ships a `.pc` file but `BuildRequires:` lacks `pkgconfig` — the \
             `pkgconfig(...)` provides generator won't run",
            span,
        ));
    }
}

impl Lint for PkgconfigFileWithoutPkgconfigBr {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn applies_to_profile(&self, profile: &Profile) -> bool {
        family_applies(profile)
    }
    fn set_profile(&mut self, profile: &Profile) {
        self.enabled = family_applies(profile);
        self.profile = profile.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;
    use rpm_spec_profile::{Family, MacroEntry, Profile, Provenance};

    fn fedora_profile() -> Profile {
        let mut p = Profile::default();
        p.identity.family = Some(Family::Fedora);
        for (n, b) in [("_prefix", "/usr"), ("_libdir", "/usr/lib64")] {
            p.macros
                .insert(n, MacroEntry::literal(b, Provenance::Override));
        }
        p
    }

    fn run(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = PkgconfigFileWithoutPkgconfigBr::new();
        lint.set_profile(&fedora_profile());
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_pc_without_pkgconfig_br() {
        let src = "Name: x\n%files\n/usr/lib64/pkgconfig/foo.pc\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM325");
    }

    #[test]
    fn silent_with_pkgconfig_br() {
        let src = "Name: x\nBuildRequires: pkgconfig\n%files\n/usr/lib64/pkgconfig/foo.pc\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_without_pc_file() {
        let src = "Name: x\n%files\n/usr/bin/foo\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn one_diagnostic_for_multiple_pc_files() {
        let src = "Name: x\n%files\n/usr/lib64/pkgconfig/foo.pc\n/usr/lib64/pkgconfig/bar.pc\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn silent_on_generic_profile() {
        // Generic / unknown family: the convention isn't established.
        let outcome = parse("Name: x\n%files\n/usr/lib64/pkgconfig/foo.pc\n");
        let mut lint = PkgconfigFileWithoutPkgconfigBr::new();
        lint.set_profile(&Profile::default());
        lint.visit_spec(&outcome.spec);
        assert!(lint.take_diagnostics().is_empty());
    }
}
