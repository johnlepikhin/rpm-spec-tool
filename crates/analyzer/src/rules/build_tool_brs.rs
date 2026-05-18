//! RPM324 `build-tool-used-without-buildrequires` — `%build`,
//! `%install`, or `%check` invokes a build helper (`cmake`, `meson`,
//! `ninja`, `pkg-config`, ...) for which the spec doesn't declare a
//! corresponding `BuildRequires:`.
//!
//! The rule keys on the family's [`PolicyRegistry::build_tool_to_buildrequires`]
//! table. Each `(command, BR-atom)` entry says "if this command shows
//! up in a build script, the BR-atom must be declared." A missing BR
//! tends to surface only on clean chroots (Mock/Koji/OBS) — the local
//! workstation already has the tool installed, so the build "works
//! for me" until CI runs.
//!
//! The check is conservative on macros: an atom whose name contains a
//! macro reference is skipped (we can't tell what it expands to). One
//! diagnostic per missing BR per spec — repeating the same tool ten
//! times in a script doesn't emit ten findings.

use std::collections::BTreeSet;

use rpm_spec::ast::{Span, SpecFile, Tag};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::policy::PolicyRegistry;
use crate::rules::util::collect_top_level_dep_names;
use crate::shell::CommandUseIndex;
use crate::visit::Visit;
use rpm_spec_profile::Profile;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM324",
    name: "build-tool-used-without-buildrequires",
    description: "A build script invokes a tool (`cmake`, `meson`, `pkg-config`, ...) without \
                  a matching `BuildRequires:`. Clean-chroot builds will fail with \
                  command-not-found.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// A build script invokes a tool (`cmake`, `meson`, `pkg-config`, ...) without a matching `BuildRequires:`. Clean-chroot builds will fail with command-not-found.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct BuildToolUsedWithoutBuildRequires {
    diagnostics: Vec<Diagnostic>,
    policy: PolicyRegistry,
}

impl BuildToolUsedWithoutBuildRequires {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for BuildToolUsedWithoutBuildRequires {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        if self.policy.build_tool_to_buildrequires.is_empty() {
            return;
        }
        let declared = collect_top_level_dep_names(spec, |t| matches!(t, Tag::BuildRequires));
        let idx = CommandUseIndex::from_spec(spec);

        // Dedup by BR-atom so repeated call sites for the same missing
        // BR emit once per spec, not per call.
        let mut reported: BTreeSet<&'static str> = BTreeSet::new();

        for call in idx.all() {
            // Only build scripts; scriptlet command requirements are
            // RPM328's territory.
            if !matches!(call.location, crate::shell::SectionRef::BuildScript { .. }) {
                continue;
            }
            let Some(cmd) = call.name.as_deref() else {
                continue;
            };
            let Some(&(_, br_atom)) = self
                .policy
                .build_tool_to_buildrequires
                .iter()
                .find(|(tool, _)| *tool == cmd)
            else {
                continue;
            };
            if declared.contains(br_atom) {
                continue;
            }
            if !reported.insert(br_atom) {
                continue;
            }
            self.diagnostics.push(Diagnostic::new(
                &METADATA,
                Severity::Warn,
                format!(
                    "build script calls `{cmd}` but the spec does not declare \
                     `BuildRequires: {br_atom}`; clean-chroot builds will fail with \
                     command-not-found"
                ),
                call.location.section_span(),
            ));
        }
    }
}

impl Lint for BuildToolUsedWithoutBuildRequires {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn applies_to_profile(&self, profile: &Profile) -> bool {
        !PolicyRegistry::for_profile(profile)
            .build_tool_to_buildrequires
            .is_empty()
    }
    fn set_profile(&mut self, profile: &Profile) {
        self.policy = PolicyRegistry::for_profile(profile);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::test_support::run_lint_with_profile;
    use rpm_spec_profile::{Family, Profile};

    fn fedora_profile() -> Profile {
        let mut p = Profile::default();
        p.identity.family = Some(Family::Fedora);
        p
    }

    fn run(src: &str) -> Vec<Diagnostic> {
        run_lint_with_profile::<BuildToolUsedWithoutBuildRequires>(src, &fedora_profile())
    }

    #[test]
    fn flags_cmake_without_br() {
        let src = "Name: x\n%build\ncmake .\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM324");
        assert!(diags[0].message.contains("cmake"));
    }

    #[test]
    fn silent_with_buildrequires() {
        let src = "Name: x\nBuildRequires: cmake\n%build\ncmake .\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_meson_in_install() {
        let src = "Name: x\n%install\nmeson install\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn silent_for_known_runtime_command() {
        let src = "Name: x\n%build\nls -la\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn deduplicates_repeated_calls() {
        // `cmake` used three times in different sections — one diag.
        let src =
            "Name: x\n%build\ncmake .\ncmake --build .\n%check\ncmake --build . --target test\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn flags_pkgconfig_call() {
        let src = "Name: x\n%build\npkg-config --cflags openssl\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("pkgconfig"));
    }

    #[test]
    fn silent_in_scriptlet() {
        // Scriptlet calls are RPM328's territory, not RPM324.
        let src = "Name: x\n%post\ncmake --version\nexit 0\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_with_rich_buildrequires() {
        // `(cmake if 0%{?fedora})` — the rich dep walker should still
        // see `cmake` as declared.
        let src =
            "Name: x\nBuildRequires: (cmake if 0%{?fedora}) and ninja-build\n%build\ncmake .\n";
        assert!(run(src).is_empty(), "rich BR should silence the rule");
    }

    #[test]
    fn silent_when_br_inside_conditional() {
        // `BuildRequires:` inside `%if`/%endif still counts.
        let src = "Name: x\n%if 0%{?fedora}\nBuildRequires: cmake\n%endif\n%build\ncmake .\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn macro_named_br_is_silently_dropped() {
        // `%{cmake_pkg}` can't be resolved literally, so the helper
        // skips it — and the rule will then fire because `cmake` isn't
        // in the declared set. Documents intentional behaviour: the
        // rule prefers a false positive over a false negative when the
        // BR name is opaque.
        let src = "Name: x\nBuildRequires: %{cmake_pkg}\n%build\ncmake .\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "macro-named BR is conservatively dropped");
    }
}
