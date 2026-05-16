//! RPM128 `group-tag-required-on-suse` — openSUSE/SLES Specfile
//! Guidelines require every package to declare a `Group:` tag. Fedora
//! formally dropped the requirement in 2019, so the rule is openSUSE-
//! exclusive.
//!
//! ## Profile gating
//!
//! `applies_to_profile` returns `true` only when
//! `family == Some(Family::Opensuse)`. Other distros skip the rule
//! entirely via the session-level filter.
//!
//! ## Trigger
//!
//! Main package preamble does not contain a `Tag::Group`. Subpackages
//! are checked separately: each `%package` block also needs its own
//! `Group:` per the same Guidelines.

use rpm_spec::ast::{Span, Tag};
use rpm_spec_profile::{Family, Profile};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::iter_packages;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM128",
    name: "group-tag-required-on-suse",
    description: "openSUSE/SLES Specfile Guidelines require every package to declare a Group: tag.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

#[derive(Debug, Default)]
pub struct GroupTagRequiredOnSuse {
    diagnostics: Vec<Diagnostic>,
}

impl GroupTagRequiredOnSuse {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for GroupTagRequiredOnSuse {
    fn visit_spec(&mut self, spec: &'ast rpm_spec::ast::SpecFile<Span>) {
        for pkg in iter_packages(spec) {
            let has_group = pkg.items().iter().any(|it| matches!(it.tag, Tag::Group));
            if has_group {
                continue;
            }
            let label = pkg
                .name()
                .map(|n| format!("package `{n}`"))
                .unwrap_or_else(|| "package".to_string());
            self.diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    METADATA.default_severity,
                    format!("{label} is missing the Group: tag (openSUSE Specfile Guidelines)"),
                    pkg.header_span(),
                )
                .with_suggestion(Suggestion::new(
                    "add a `Group:` tag matching the openSUSE category hierarchy \
                     (e.g. `System/Libraries`, `Development/Tools`)",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

impl Lint for GroupTagRequiredOnSuse {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }

    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    fn applies_to_profile(&self, profile: &Profile) -> bool {
        matches!(profile.identity.family, Some(Family::Opensuse))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::util::make_test_profile;
    use crate::session::parse;

    fn run(src: &str, profile: &Profile) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = GroupTagRequiredOnSuse::new();
        if !lint.applies_to_profile(profile) {
            return Vec::new();
        }
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn fires_on_suse_without_group() {
        let profile = make_test_profile(Some(Family::Opensuse), None, &[], &[]);
        let src = "Name: x\nVersion: 1\nRelease: 1\nLicense: MIT\n";
        let diags = run(src, &profile);
        assert_eq!(diags.len(), 1, "got {diags:?}");
        assert_eq!(diags[0].lint_id, "RPM128");
        assert!(diags[0].message.contains("Group:"));
    }

    #[test]
    fn silent_on_suse_with_group() {
        let profile = make_test_profile(Some(Family::Opensuse), None, &[], &[]);
        let src = "Name: x\nGroup: System/Libraries\n";
        assert!(run(src, &profile).is_empty());
    }

    #[test]
    fn silent_on_fedora_without_group() {
        let profile = make_test_profile(Some(Family::Fedora), Some(".fc40"), &[], &[]);
        let src = "Name: x\nLicense: MIT\n";
        assert!(
            run(src, &profile).is_empty(),
            "Fedora dropped Group: requirement"
        );
    }

    #[test]
    fn silent_on_alt_without_group() {
        let profile = make_test_profile(Some(Family::Alt), None, &[], &[]);
        assert!(run("Name: x\nLicense: MIT\n", &profile).is_empty());
    }

    #[test]
    fn silent_when_no_family() {
        let profile = make_test_profile(None, None, &[], &[]);
        assert!(run("Name: x\nLicense: MIT\n", &profile).is_empty());
    }

    #[test]
    fn flags_subpackage_missing_group_on_suse() {
        let profile = make_test_profile(Some(Family::Opensuse), None, &[], &[]);
        // Main has Group, subpackage doesn't — only one diagnostic.
        let src = "\
Name: x
Version: 1
Release: 1
License: MIT
Group: System/Libraries
Summary: s

%description
b

%package devel
Summary: dev

%description devel
d
";
        let diags = run(src, &profile);
        assert_eq!(
            diags.len(),
            1,
            "subpackage missing Group should fire; got {diags:?}"
        );
        assert!(
            diags[0].message.contains("x-devel") || diags[0].message.contains("package"),
            "expected name reference; got {}",
            diags[0].message
        );
    }
}
