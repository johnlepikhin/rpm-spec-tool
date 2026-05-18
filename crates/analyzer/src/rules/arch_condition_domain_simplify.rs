//! RPM440 `arch-condition-domain-simplify` — flag `%ifarch` lists that
//! cover the entire target arch universe of the active profile.
//!
//! If the profile only ever targets `{x86_64, aarch64}`, then writing
//! `%ifarch x86_64 aarch64` is the same as `%if 1` — every build hits
//! the branch.
//!
//! Silent when the profile has no declared arch universe
//! ([`rpm_spec_profile::Profile::arch_universe`] returns `None`) — we
//! refuse to claim "covers the universe" without one.

use std::collections::BTreeSet;

use rpm_spec::ast::{
    CondExpr, CondKind, Conditional, FilesContent, PreambleContent, Span, SpecItem,
};
use rpm_spec_profile::Profile;

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::literal_archs;
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM440",
    name: "arch-condition-domain-simplify",
    description: "`%ifarch <list>` covers every architecture the active profile may target — \
                  the condition is always true; drop the `%ifarch` wrapper.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// `%ifarch <list>` covers every architecture the active profile may target — the condition is always true; drop the `%ifarch` wrapper.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct ArchConditionDomainSimplify {
    diagnostics: Vec<Diagnostic>,
    universe: BTreeSet<String>,
}

impl ArchConditionDomainSimplify {
    pub fn new() -> Self {
        Self::default()
    }

    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        if self.universe.is_empty() {
            return;
        }
        for branch in &node.branches {
            if !matches!(branch.kind, CondKind::IfArch | CondKind::ElifArch) {
                continue;
            }
            let CondExpr::ArchList(list) = &branch.expr else {
                continue;
            };
            let Some(archs) = literal_archs(list) else {
                continue;
            };
            if !self.universe.is_subset(&archs) {
                continue;
            }
            self.diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    format!(
                        "`%ifarch` list covers every arch in the profile's target universe \
                         ({n}); the condition is always true",
                        n = self.universe.len()
                    ),
                    branch.data,
                )
                .with_suggestion(Suggestion::new(
                    "drop the `%ifarch` wrapper; the body always runs",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

impl<'ast> Visit<'ast> for ArchConditionDomainSimplify {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        self.check(node);
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        self.check(node);
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        self.check(node);
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for ArchConditionDomainSimplify {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_profile(&mut self, profile: &Profile) {
        self.universe = profile.arch_universe().cloned().unwrap_or_default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn build_profile_with_universe(archs: &[&str]) -> Profile {
        use rpm_spec_profile::merge::{ArchPatch, ProfilePatch};
        let mut p = Profile::default();
        let universe: BTreeSet<String> = archs.iter().map(|a| (*a).to_string()).collect();
        let arch_patch = ArchPatch {
            target_arch_universe: Some(universe),
            ..Default::default()
        };
        p.apply(ProfilePatch {
            arch: arch_patch,
            ..Default::default()
        });
        p
    }

    fn run(src: &str, archs: &[&str]) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let profile = build_profile_with_universe(archs);
        let mut lint = ArchConditionDomainSimplify::new();
        lint.set_profile(&profile);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_full_universe_cover() {
        let src = "Name: x\n%ifarch x86_64 aarch64\nLicense: MIT\n%endif\n";
        let diags = run(src, &["x86_64", "aarch64"]);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM440");
    }

    #[test]
    fn flags_superset_cover() {
        // Universe = {x86_64, aarch64}; list includes ppc64le too.
        let src = "Name: x\n%ifarch x86_64 aarch64 ppc64le\nLicense: MIT\n%endif\n";
        let diags = run(src, &["x86_64", "aarch64"]);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_partial_cover() {
        let src = "Name: x\n%ifarch x86_64\nLicense: MIT\n%endif\n";
        let diags = run(src, &["x86_64", "aarch64"]);
        assert!(diags.is_empty());
    }

    #[test]
    fn silent_when_profile_has_no_universe() {
        // No `target_arch_universe` → rule stays silent.
        let src = "Name: x\n%ifarch x86_64 aarch64\nLicense: MIT\n%endif\n";
        let outcome = parse(src);
        let profile = Profile::default();
        let mut lint = ArchConditionDomainSimplify::new();
        lint.set_profile(&profile);
        lint.visit_spec(&outcome.spec);
        assert!(lint.take_diagnostics().is_empty());
    }

    #[test]
    fn silent_for_ifnarch() {
        // RPM440 only handles `%ifarch`, not `%ifnarch` (different semantics).
        let src = "Name: x\n%ifnarch x86_64 aarch64\nLicense: MIT\n%endif\n";
        let diags = run(src, &["x86_64", "aarch64"]);
        assert!(diags.is_empty());
    }
}
