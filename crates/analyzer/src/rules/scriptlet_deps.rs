//! RPM328 `scriptlet-command-without-requires` — a scriptlet invokes
//! a runtime helper (`useradd`, `getent`, `update-alternatives`, ...)
//! whose providing package isn't declared in `Requires:` (any
//! qualifier).
//!
//! The policy table lives on [`PolicyRegistry::scriptlet_required_deps`]
//! and lists `(command, providing-atom)` pairs that every Fedora-,
//! openSUSE-, and ALT-style packaging guide treats as mandatory: when
//! a `%pre` or `%post` runs `useradd`, the spec must `Requires(pre):
//! shadow-utils` (or its family equivalent). Without the dep, the
//! scriptlet aborts on minimal images that don't pull the helper in
//! transitively.
//!
//! We accept *any* qualifier on the matching `Requires:` — a plain
//! `Requires: shadow-utils` is fine, as is `Requires(pre,post)`. The
//! diagnostic reports the *canonical* qualifier matching the scriptlet
//! phase (e.g. `Requires(pre)` for `%pre`), which is the form the user
//! will most likely want to type. The rule fires once per
//! `(scriptlet-section, missing-atom)` pair so a `%post` that calls
//! `systemctl` three times produces one diagnostic, not three.
//! `systemctl` calls themselves are also flagged by RPM342 ("use
//! `%systemd_post`"); the two rules are complementary — RPM342 says
//! "wrong tool", RPM328 says "missing dep if you keep the tool".

use std::collections::BTreeSet;

use rpm_spec::ast::{ScriptletKind, Span, SpecFile, Tag};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::policy::PolicyRegistry;
use crate::rules::util::collect_top_level_dep_names;
use crate::shell::{CommandUseIndex, SectionRef};
use crate::visit::Visit;
use rpm_spec_profile::Profile;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM328",
    name: "scriptlet-command-without-requires",
    description: "A scriptlet invokes a runtime helper (`useradd`, `getent`, \
                  `update-alternatives`, ...) without declaring the providing package in \
                  `Requires:`. Minimal images abort the scriptlet with command-not-found.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct ScriptletCommandWithoutRequires {
    diagnostics: Vec<Diagnostic>,
    policy: PolicyRegistry,
}

impl ScriptletCommandWithoutRequires {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for ScriptletCommandWithoutRequires {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        if self.policy.scriptlet_required_deps.is_empty() {
            return;
        }
        let declared = collect_top_level_dep_names(spec, |t| matches!(t, Tag::Requires));
        let idx = CommandUseIndex::from_spec(spec);
        let mut reported: BTreeSet<(usize, &'static str)> = BTreeSet::new();

        for call in idx.all() {
            let SectionRef::Scriptlet { kind, .. } = call.location else {
                continue;
            };
            let Some(cmd) = call.name.as_deref() else {
                continue;
            };
            let Some(&(_, req_atom)) = self
                .policy
                .scriptlet_required_deps
                .iter()
                .find(|(tool, _)| *tool == cmd)
            else {
                continue;
            };
            if declared.contains(req_atom) {
                continue;
            }
            // Dedup per `(scriptlet section, missing atom)` so multiple
            // calls to the same tool in one scriptlet emit once.
            let key = (call.location.section_span().start_byte, req_atom);
            if !reported.insert(key) {
                continue;
            }
            let phase = scriptlet_qualifier(kind);
            self.diagnostics.push(Diagnostic::new(
                &METADATA,
                Severity::Warn,
                format!(
                    "scriptlet calls `{cmd}` in %{phase} but the spec does not declare \
                     `Requires({phase}): {req_atom}`; the scriptlet will abort on minimal \
                     images that don't pull the helper in transitively"
                ),
                call.location.section_span(),
            ));
        }
    }
}

/// Canonical `Requires(<qualifier>)` token matching the scriptlet
/// phase. Mirrors RPM's own naming so the diagnostic suggestion is
/// directly copy-pasteable into the spec.
fn scriptlet_qualifier(kind: ScriptletKind) -> &'static str {
    match kind {
        ScriptletKind::Pre => "pre",
        ScriptletKind::Post => "post",
        ScriptletKind::Preun => "preun",
        ScriptletKind::Postun => "postun",
        ScriptletKind::Pretrans => "pretrans",
        ScriptletKind::Posttrans => "posttrans",
        ScriptletKind::Preuntrans => "preuntrans",
        ScriptletKind::Postuntrans => "postuntrans",
        // `ScriptletKind` is `#[non_exhaustive]`; a future variant
        // falls back to a neutral label that still reads as a hint.
        _ => "pre",
    }
}

impl Lint for ScriptletCommandWithoutRequires {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn applies_to_profile(&self, profile: &Profile) -> bool {
        !PolicyRegistry::for_profile(profile)
            .scriptlet_required_deps
            .is_empty()
    }
    fn set_profile(&mut self, profile: &Profile) {
        self.policy = PolicyRegistry::for_profile(profile);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;
    use rpm_spec_profile::{Family, Profile};

    fn fedora_profile() -> Profile {
        let mut p = Profile::default();
        p.identity.family = Some(Family::Fedora);
        p
    }

    fn run(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = ScriptletCommandWithoutRequires::new();
        lint.set_profile(&fedora_profile());
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_useradd_without_requires() {
        let src = "Name: x\n%pre\nuseradd -r foo\nexit 0\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM328");
        assert!(diags[0].message.contains("useradd"));
        assert!(diags[0].message.contains("shadow-utils"));
        assert!(
            diags[0].message.contains("Requires(pre)"),
            "should suggest the matching qualifier: {:?}",
            diags[0].message
        );
    }

    #[test]
    fn silent_when_requires_declared() {
        let src = "Name: x\nRequires(pre): shadow-utils\n%pre\nuseradd -r foo\nexit 0\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_plain_requires_declared() {
        // Any qualifier flavour of `Requires:` covers the scriptlet.
        let src = "Name: x\nRequires: shadow-utils\n%pre\nuseradd -r foo\nexit 0\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_requires_has_qualifier_list() {
        // `Requires(pre,post): shadow-utils` — combined qualifier list
        // must still register as declared.
        let src = "Name: x\nRequires(pre,post): shadow-utils\n%pre\nuseradd -r foo\nexit 0\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_systemctl_in_post() {
        // RPM328 fires; RPM342 also fires in the registry, but we test
        // RPM328 in isolation here.
        let src = "Name: x\n%post\nsystemctl daemon-reload\nexit 0\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("systemd"));
        assert!(diags[0].message.contains("Requires(post)"));
    }

    #[test]
    fn silent_in_buildscript() {
        // %install isn't a scriptlet — RPM324 owns that surface.
        let src = "Name: x\n%install\nuseradd -r foo\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn deduplicates_repeated_calls_in_one_scriptlet() {
        let src = "Name: x\n%post\nsystemctl daemon-reload\nsystemctl restart foo\nexit 0\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn emits_per_scriptlet_section() {
        // userdel isn't in the policy table; only %pre fires.
        let src = "Name: x\n%pre\nuseradd -r foo\nexit 0\n%postun\nuserdel foo\nexit 0\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn emits_once_per_scriptlet_per_atom() {
        // `useradd` and `groupadd` both → `shadow-utils`; one
        // scriptlet, one atom — one diagnostic.
        let src = "Name: x\n%pre\nuseradd -r foo\ngroupadd -r foo\nexit 0\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn silent_for_unknown_command() {
        let src = "Name: x\n%post\nfoo --bar\nexit 0\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_on_generic_profile() {
        let outcome = parse("Name: x\n%pre\nuseradd -r foo\n");
        let mut lint = ScriptletCommandWithoutRequires::new();
        lint.set_profile(&Profile::default());
        lint.visit_spec(&outcome.spec);
        assert!(lint.take_diagnostics().is_empty());
    }
}
