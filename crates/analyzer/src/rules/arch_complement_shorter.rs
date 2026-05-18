//! RPM441 `arch-complement-shorter` — flag `%ifnarch` lists whose
//! complement against the profile's target arch universe is shorter.
//!
//! `%ifnarch i686 ppc64le s390x armv7hl mips` against a universe of
//! eight arches can be rewritten as `%ifarch x86_64 aarch64 …` with
//! the complement on the positive side. The flipped form is shorter
//! to read and easier to audit.
//!
//! Silent when the profile has no declared arch universe.

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
    id: "RPM441",
    name: "arch-complement-shorter",
    description: "`%ifnarch` lists more arches than the complement against the profile's target \
                  universe — flip to `%ifarch` with the complement set.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// `%ifnarch` lists more arches than the complement against the profile's target universe — flip to `%ifarch` with the complement set.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct ArchComplementShorter {
    diagnostics: Vec<Diagnostic>,
    universe: BTreeSet<String>,
}

impl ArchComplementShorter {
    pub fn new() -> Self {
        Self::default()
    }

    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        if self.universe.is_empty() {
            return;
        }
        for branch in &node.branches {
            if !matches!(branch.kind, CondKind::IfNArch | CondKind::ElifOs) {
                continue;
            }
            let CondExpr::ArchList(list) = &branch.expr else {
                continue;
            };
            let Some(archs) = literal_archs(list) else {
                continue;
            };
            // Every excluded arch must be in the universe; otherwise the
            // complement isn't a clean rewrite (we'd lose information).
            if !archs.is_subset(&self.universe) {
                continue;
            }
            let complement: BTreeSet<&String> = self.universe.difference(&archs).collect();
            if complement.is_empty() {
                // Whole universe excluded → always false (RPM112-ish).
                continue;
            }
            if complement.len() >= archs.len() {
                // Flipping wouldn't shorten the form.
                continue;
            }
            self.diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    format!(
                        "`%ifnarch` excludes {n} arches; the complement against the profile \
                         universe is {k} arches — flip to `%ifarch`",
                        n = archs.len(),
                        k = complement.len()
                    ),
                    branch.data,
                )
                .with_suggestion(Suggestion::new(
                    "rewrite as `%ifarch` listing the complement",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

impl<'ast> Visit<'ast> for ArchComplementShorter {
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

impl Lint for ArchComplementShorter {
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
        let mut lint = ArchComplementShorter::new();
        lint.set_profile(&profile);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_when_complement_is_shorter() {
        // Universe = 5 arches; exclude 3, complement is 2 (shorter).
        let src = "Name: x\n%ifnarch i686 ppc64le s390x\nLicense: MIT\n%endif\n";
        let diags = run(src, &["x86_64", "aarch64", "i686", "ppc64le", "s390x"]);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM441");
    }

    #[test]
    fn silent_when_complement_is_longer() {
        // Exclude 2; complement is 3 — flipping doesn't help.
        let src = "Name: x\n%ifnarch i686 ppc64le\nLicense: MIT\n%endif\n";
        let diags = run(src, &["x86_64", "aarch64", "i686", "ppc64le", "s390x"]);
        assert!(diags.is_empty());
    }

    #[test]
    fn silent_when_excluded_outside_universe() {
        // `ppc64le` not in universe → don't claim the flip is sound.
        let src = "Name: x\n%ifnarch i686 ppc64le\nLicense: MIT\n%endif\n";
        let diags = run(src, &["x86_64", "aarch64"]);
        assert!(diags.is_empty());
    }

    #[test]
    fn silent_for_ifarch() {
        let src = "Name: x\n%ifarch i686\nLicense: MIT\n%endif\n";
        let diags = run(src, &["x86_64", "aarch64", "i686"]);
        assert!(diags.is_empty());
    }

    #[test]
    fn silent_when_profile_has_no_universe() {
        let src = "Name: x\n%ifnarch i686 ppc64le s390x\nLicense: MIT\n%endif\n";
        let outcome = parse(src);
        let profile = Profile::default();
        let mut lint = ArchComplementShorter::new();
        lint.set_profile(&profile);
        lint.visit_spec(&outcome.spec);
        assert!(lint.take_diagnostics().is_empty());
    }
}
