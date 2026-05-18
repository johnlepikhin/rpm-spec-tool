//! RPM021 `deprecated-clean-section` — `%clean` is unnecessary in modern
//! rpm: the buildroot is cleaned automatically before each build. Fedora,
//! openSUSE and others explicitly say not to include one.
//!
//! ## Profile gating
//!
//! The diagnostic always fires; the **auto-fix** (drop the section) is
//! attached as `MachineApplicable` only when the active profile is one
//! where `%clean` is definitively obsolete — `Family::Fedora` or
//! `Family::Rhel`, plus the unknown-family case (no profile info → keep
//! current safe behaviour). On SLES / SUSE / older ALT setups some
//! packagers still rely on `%clean`, so we degrade to a `Manual` hint
//! rather than offering an automatic deletion that might break their
//! build.

use rpm_spec::ast::{BuildScriptKind, Section, Span};
use rpm_spec_profile::{Family, Profile};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::drop_span;
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM021",
    name: "deprecated-clean-section",
    description: "The %clean section is unnecessary; modern rpm cleans the buildroot automatically.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

/// The %clean section is unnecessary; modern rpm cleans the buildroot automatically.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct DeprecatedCleanSection {
    diagnostics: Vec<Diagnostic>,
    /// Recorded from `Lint::set_profile`. Controls whether the
    /// "drop the section" suggestion is `MachineApplicable` (safe on
    /// distros where `%clean` is fully obsolete) or downgraded to a
    /// `Manual` hint (where some packagers still rely on the section).
    family: Option<Family>,
}

impl DeprecatedCleanSection {
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` when the suggestion can be applied automatically — i.e.
    /// the distro family is one where modern rpm runs an implicit clean
    /// and the section is unambiguously redundant. `None` (unknown
    /// family) defaults to safe-to-fix to preserve pre-profile-aware
    /// behaviour.
    fn autofix_safe(&self) -> bool {
        match self.family {
            Some(Family::Fedora | Family::Rhel) | None => true,
            // SUSE / ALT / Mageia / Generic and any future variant —
            // be conservative and let the user decide.
            _ => false,
        }
    }
}

impl<'ast> Visit<'ast> for DeprecatedCleanSection {
    fn visit_section(&mut self, node: &'ast Section<Span>) {
        if let Section::BuildScript {
            kind: BuildScriptKind::Clean,
            data,
            ..
        } = node
        {
            let mut diag = Diagnostic::new(
                &METADATA,
                Severity::Warn,
                "%clean section is no longer needed",
                *data,
            );
            diag = if self.autofix_safe() {
                diag.with_suggestion(Suggestion::new(
                    "remove the %clean section",
                    vec![drop_span(*data)],
                    Applicability::MachineApplicable,
                ))
            } else {
                // Distro-specific: don't ship an automatic deletion;
                // hint at manual inspection.
                diag.with_suggestion(Suggestion::new(
                    "consider removing the %clean section (your distro may still rely on it; \
                     review before applying)",
                    Vec::new(),
                    Applicability::Manual,
                ))
            };
            self.diagnostics.push(diag);
        }
        visit::walk_section(self, node);
    }
}

impl Lint for DeprecatedCleanSection {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }

    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    fn set_profile(&mut self, profile: &Profile) {
        self.family = profile.identity.family;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::test_support::run_lint;
    use crate::session::parse;

    fn run(src: &str) -> Vec<Diagnostic> {
        run_lint::<DeprecatedCleanSection>(src)
    }

    fn run_with_family(src: &str, family: Option<Family>) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = DeprecatedCleanSection::new();
        let mut profile = Profile::default();
        profile.identity.family = family;
        lint.set_profile(&profile);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_clean_section() {
        let src = "Name: x\n%clean\nrm -rf %{buildroot}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM021");
        // No profile set → autofix_safe() defaults to true.
        assert!(!diags[0].suggestions.is_empty());
        assert_eq!(
            diags[0].suggestions[0].applicability,
            Applicability::MachineApplicable
        );
    }

    #[test]
    fn silent_when_no_clean_section() {
        let src = "Name: x\n%build\nmake\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn fedora_autofix_is_machine_applicable() {
        let diags = run_with_family(
            "Name: x\n%clean\nrm -rf %{buildroot}\n",
            Some(Family::Fedora),
        );
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].suggestions[0].applicability,
            Applicability::MachineApplicable,
            "Fedora: %clean is obsolete; autofix is safe"
        );
    }

    #[test]
    fn rhel_autofix_is_machine_applicable() {
        let diags = run_with_family("Name: x\n%clean\nrm -rf %{buildroot}\n", Some(Family::Rhel));
        assert_eq!(
            diags[0].suggestions[0].applicability,
            Applicability::MachineApplicable
        );
    }

    #[test]
    fn opensuse_degrades_to_manual_hint() {
        let diags = run_with_family(
            "Name: x\n%clean\nrm -rf %{buildroot}\n",
            Some(Family::Opensuse),
        );
        assert_eq!(diags.len(), 1, "warning still fires");
        assert_eq!(
            diags[0].suggestions[0].applicability,
            Applicability::Manual,
            "openSUSE: don't auto-delete — some specs still rely on %clean"
        );
        // No edits attached on the Manual path.
        assert!(diags[0].suggestions[0].edits.is_empty());
    }

    #[test]
    fn alt_degrades_to_manual_hint() {
        let diags = run_with_family("Name: x\n%clean\nrm -rf %{buildroot}\n", Some(Family::Alt));
        assert_eq!(diags[0].suggestions[0].applicability, Applicability::Manual);
    }

    #[test]
    fn unknown_family_preserves_legacy_autofix() {
        // When no profile is loaded (or family auto-detect failed),
        // keep the historical machine-applicable fix so pre-profile
        // pipelines don't regress.
        let diags = run_with_family("Name: x\n%clean\nrm -rf %{buildroot}\n", None);
        assert_eq!(
            diags[0].suggestions[0].applicability,
            Applicability::MachineApplicable
        );
    }
}
