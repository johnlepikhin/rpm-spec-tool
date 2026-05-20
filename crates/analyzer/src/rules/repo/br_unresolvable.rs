//! RPM-REPO-001 `buildrequires-unresolvable` — a `BuildRequires:`
//! atom has no provider in any configured repo of the active
//! profile.
//!
//! On clean-chroot builders (Mock / Koji / OBS / hasher) an unmet
//! BR is the difference between a build that runs and a build that
//! aborts with "nothing provides foo needed by ...". This rule
//! catches the case at lint time, before the chroot ever spins up,
//! by checking each declared BR against the resolved
//! [`rpm_spec_repo_core::RepoUniverse`] for the profile.
//!
//! Severity default `deny`: an unresolvable BR is always a build
//! failure, not a stylistic concern. Skipped silently when no
//! repos are configured for the profile (the lint can't say
//! anything meaningful).

use std::sync::Arc;

use rpm_spec::ast::{Span, SpecFile, Tag};
use rpm_spec_profile::Profile;
use rpm_spec_repo_core::RepoUniverse;
use rpm_spec_repo_resolver::{LookupOutcome, lookup};

use crate::diagnostic::{Diagnostic, LintCategory, RepoContext, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

use super::shared::RepoRule;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM-REPO-001",
    name: "buildrequires-unresolvable",
    description: "A `BuildRequires:` atom has no provider in any configured repo for the \
                  active profile. Clean-chroot builds will fail with \
                  \"nothing provides ...\".",
    default_severity: Severity::Deny,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct BuildRequiresUnresolvable {
    base: RepoRule,
}

impl BuildRequiresUnresolvable {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for BuildRequiresUnresolvable {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        self.base.walk_deps(
            spec,
            |t| matches!(t, Tag::BuildRequires),
            |state, dep, diagnostics| {
                let outcome = match lookup(&state.universe, &dep.requirement) {
                    Ok(o) => o,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            dep = %dep.display,
                            "repo lookup failed; skipping atom",
                        );
                        return;
                    }
                };
                match outcome {
                    LookupOutcome::Satisfied { .. } => {}
                    LookupOutcome::VersionUnsatisfied { .. } => {
                        // Version-side failures are RPM-REPO-003's
                        // territory; this rule fires only when the name
                        // has zero providers.
                    }
                    LookupOutcome::NoProvider => {
                        diagnostics.push(
                            Diagnostic::new(
                                &METADATA,
                                METADATA.default_severity,
                                format!(
                                    "no provider in any configured repo for `BuildRequires: {}`; \
                                     clean-chroot builds will fail with `nothing provides ...`",
                                    dep.display
                                ),
                                dep.span,
                            )
                            .with_repo_context(
                                RepoContext::for_profile(&state.universe.profile_name),
                            ),
                        );
                    }
                }
            },
        );
    }
}

impl Lint for BuildRequiresUnresolvable {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        self.base.take_diagnostics()
    }
    fn set_profile(&mut self, profile: &Profile) {
        self.base.set_profile(profile);
    }
    fn set_repo_universe(&mut self, universe: Option<Arc<RepoUniverse>>) {
        self.base.set_repo_universe(universe);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::repo::test_fixtures::{redos_profile, tiny_universe};
    use crate::rules::test_support::run_repo_lint;

    #[test]
    fn flags_missing_buildrequires() {
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\nBuildRequires: missing-package\n%description\nx\n";
        let diags = run_repo_lint::<BuildRequiresUnresolvable>(
            src,
            &redos_profile(),
            tiny_universe(),
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM-REPO-001");
        assert!(diags[0].message.contains("missing-package"), "{}", diags[0].message);
        assert!(diags[0].repo_context.is_some());
    }

    #[test]
    fn silent_when_buildrequire_present() {
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\nBuildRequires: bash\n%description\nx\n";
        let diags = run_repo_lint::<BuildRequiresUnresolvable>(
            src,
            &redos_profile(),
            tiny_universe(),
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn skips_buildrequires_inside_inactive_vendor_branch() {
        // `%_vendor` in the redos profile resolves to "redhat", so the
        // ROSA-guarded `lib64systemd-devel` line must NOT be reported
        // as missing — that branch never fires. The `%else` branch's
        // BR (`bash`) IS satisfied by the tiny universe, so we expect
        // zero diagnostics overall.
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\n\
                   %if \"%_vendor\" == \"rosa\"\n\
                   BuildRequires: lib64systemd-devel\n\
                   %else\n\
                   BuildRequires: bash\n\
                   %endif\n\
                   %description\nx\n";
        // Need a profile whose `_vendor` macro is set to "redhat" —
        // the test fixture profile carries the redos identity but
        // not the showrc macros, so inject `_vendor` explicitly.
        let mut profile = redos_profile();
        profile.macros.insert(
            "_vendor".to_string(),
            rpm_spec_profile::MacroEntry::literal(
                "redhat",
                rpm_spec_profile::Provenance::Override,
            ),
        );
        let diags =
            run_repo_lint::<BuildRequiresUnresolvable>(src, &profile, tiny_universe());
        assert!(
            diags.is_empty(),
            "inactive vendor branch should be skipped; got {diags:?}",
        );
    }

    #[test]
    fn flags_buildrequires_inside_active_vendor_branch() {
        // Same shape, but now the test profile pretends to BE rosa so
        // the `lib64systemd-devel` arm fires — that atom is missing
        // from the tiny universe and must produce one diagnostic.
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\n\
                   %if \"%_vendor\" == \"rosa\"\n\
                   BuildRequires: lib64systemd-devel\n\
                   %else\n\
                   BuildRequires: bash\n\
                   %endif\n\
                   %description\nx\n";
        let mut profile = redos_profile();
        profile.macros.insert(
            "_vendor".to_string(),
            rpm_spec_profile::MacroEntry::literal(
                "rosa",
                rpm_spec_profile::Provenance::Override,
            ),
        );
        let diags =
            run_repo_lint::<BuildRequiresUnresolvable>(src, &profile, tiny_universe());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(
            diags[0].message.contains("lib64systemd-devel"),
            "{}",
            diags[0].message
        );
    }

    #[test]
    fn silent_when_universe_missing() {
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\nBuildRequires: missing-package\n%description\nx\n";
        // Skip the universe entirely — this models a profile that has
        // no repos configured (or all snapshots are cache-miss). The
        // rule must produce zero diagnostics.
        let outcome = crate::session::parse(src);
        let mut lint = BuildRequiresUnresolvable::default();
        lint.set_profile(&redos_profile());
        lint.set_repo_universe(None);
        lint.visit_spec(&outcome.spec);
        assert!(lint.take_diagnostics().is_empty());
    }
}
