//! RPM-REPO-003 `buildrequires-version-unsatisfied` — a
//! `BuildRequires:` atom names a provider that EXISTS in the
//! configured repos, but none of the available versions satisfy
//! the spec's constraint.
//!
//! Most commonly the spec demands a newer release than the repo
//! ships (`BuildRequires: cmake >= 3.28` but the repo has
//! `cmake-3.20`). The lint reports the best available EVR so the
//! packager sees the gap immediately.
//!
//! Severity default `warn`: the build will still fail, but the
//! diagnostic message points the user at a concrete remediation
//! (drop the constraint, or guard with `%if 0%{?distro_version} >=
//! N`). Promote to `deny` via `.rpmspec.toml` if you want this
//! treated as a hard block in CI.

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
    id: "RPM-REPO-003",
    name: "buildrequires-version-unsatisfied",
    description: "A `BuildRequires:` atom names a package that exists in the configured \
                  repos, but the version constraint is not met by any available release.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct BuildRequiresVersionUnsatisfied {
    base: RepoRule,
}

impl BuildRequiresVersionUnsatisfied {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for BuildRequiresVersionUnsatisfied {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        self.base.walk_deps(
            spec,
            |t| matches!(t, Tag::BuildRequires),
            |state, dep, diagnostics| {
                let outcome = match lookup(&state.universe, &dep.capability) {
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
                if let LookupOutcome::VersionUnsatisfied {
                    best_available,
                    best_provider,
                } = outcome
                {
                    let best_str = best_available.to_string();
                    let nevra_str = best_provider.to_string();
                    let ctx = RepoContext::for_profile(&state.universe.profile_name)
                        .with_nevra(&nevra_str);
                    diagnostics.push(
                        Diagnostic::new(
                            &METADATA,
                            METADATA.default_severity,
                            format!(
                                "configured repos provide `{name}` but none satisfy `{display}`; \
                                 best available is `{best_str}` (from `{nevra_str}`)",
                                name = dep.capability.name,
                                display = dep.display,
                            ),
                            dep.span,
                        )
                        .with_repo_context(ctx),
                    );
                }
            },
        );
    }
}

impl Lint for BuildRequiresVersionUnsatisfied {
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
    fn flags_version_too_old() {
        // Tiny universe ships cmake 3.20 + 3.26; require 3.28.
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\nBuildRequires: cmake >= 3.28\n%description\nx\n";
        let diags = run_repo_lint::<BuildRequiresVersionUnsatisfied>(
            src,
            &redos_profile(),
            tiny_universe(),
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM-REPO-003");
        assert!(diags[0].message.contains("3.26.5"), "{}", diags[0].message);
    }

    #[test]
    fn silent_when_version_satisfied() {
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\nBuildRequires: cmake >= 3.20\n%description\nx\n";
        let diags = run_repo_lint::<BuildRequiresVersionUnsatisfied>(
            src,
            &redos_profile(),
            tiny_universe(),
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn ignores_no_provider() {
        // RPM-REPO-001's territory — this rule must stay silent.
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\nBuildRequires: missing-package >= 1\n%description\nx\n";
        let diags = run_repo_lint::<BuildRequiresVersionUnsatisfied>(
            src,
            &redos_profile(),
            tiny_universe(),
        );
        assert!(diags.is_empty(), "{diags:?}");
    }
}
