//! RPM-REPO-002 `runtime-requires-unresolvable` — a `Requires:`
//! atom has no provider in any configured repo of the active
//! profile.
//!
//! Less severe than RPM-REPO-001: the spec builds fine, but the
//! resulting RPM won't install on a target system that only has
//! the configured repos. Tracked separately so packagers can warn
//! on missing runtime deps without making the build itself fail.

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
    id: "RPM-REPO-002",
    name: "runtime-requires-unresolvable",
    description: "A `Requires:` atom has no provider in any configured repo for the active \
                  profile. The package will build but won't install on a target with only \
                  these repos.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct RuntimeRequiresUnresolvable {
    base: RepoRule,
}

impl RuntimeRequiresUnresolvable {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for RuntimeRequiresUnresolvable {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        self.base.walk_deps(
            spec,
            |t| matches!(t, Tag::Requires),
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
                if let LookupOutcome::NoProvider = outcome {
                    diagnostics.push(
                        Diagnostic::new(
                            &METADATA,
                            METADATA.default_severity,
                            format!(
                                "no provider in any configured repo for `Requires: {}`; \
                                 the built RPM will fail to install on systems with only \
                                 these repos",
                                dep.display
                            ),
                            dep.span,
                        )
                        .with_repo_context(RepoContext::for_profile(
                            &state.universe.profile_name,
                        )),
                    );
                }
            },
        );
    }
}

impl Lint for RuntimeRequiresUnresolvable {
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
    fn flags_missing_runtime_requires() {
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\nRequires: missing-package\n%description\nx\n";
        let diags =
            run_repo_lint::<RuntimeRequiresUnresolvable>(src, &redos_profile(), tiny_universe());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM-REPO-002");
    }

    #[test]
    fn silent_when_runtime_requires_present() {
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\nRequires: glibc\n%description\nx\n";
        let diags =
            run_repo_lint::<RuntimeRequiresUnresolvable>(src, &redos_profile(), tiny_universe());
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn ignores_buildrequires() {
        // RPM-REPO-002 is the runtime-only counterpart of RPM-REPO-001;
        // a missing BuildRequires must NOT trigger this rule.
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\nBuildRequires: missing-package\n%description\nx\n";
        let diags =
            run_repo_lint::<RuntimeRequiresUnresolvable>(src, &redos_profile(), tiny_universe());
        assert!(diags.is_empty(), "{diags:?}");
    }
}
