//! RPM347 `tmpfiles-without-create` — `%files` ships a tmpfiles.d
//! drop-in but no scriptlet invokes the distro's `%tmpfiles_create*`
//! macro.
//!
//! tmpfiles.d entries describe runtime state that `systemd-tmpfiles`
//! materialises on boot. RPM installs the drop-in but `tmpfiles-create`
//! must run *now* (post-install) so the paths exist for the package
//! that just landed — otherwise its `%post`/runtime trips on missing
//! directories until the next reboot.
//!
//! Family-gated: only distros whose policy registry exposes a
//! `tmpfiles_create` macro produce a diagnostic; on Generic the rule
//! stays silent.

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::files::{FilesClassifier, for_each_files_entry};
use crate::lint::{Lint, LintMetadata};
use crate::policy::{PolicyRegistry, line_references_any_macro};
use crate::shell::for_each_scriptlet;
use crate::visit::Visit;
use rpm_spec_profile::Profile;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM347",
    name: "tmpfiles-without-create",
    description: "`%files` includes a `tmpfiles.d/*.conf` drop-in but no scriptlet runs the \
                  distro's `%tmpfiles_create*` macro. The directories described by the \
                  drop-in won't exist until the next reboot.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

#[derive(Debug, Default)]
pub struct TmpfilesWithoutCreate {
    diagnostics: Vec<Diagnostic>,
    profile: Profile,
    policy: PolicyRegistry,
}

impl TmpfilesWithoutCreate {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for TmpfilesWithoutCreate {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        if self.policy.tmpfiles_create_macros.is_empty() {
            return;
        }
        let classifier = FilesClassifier::new(&self.profile);
        let mut first_tmpfiles_span: Option<Span> = None;
        for_each_files_entry(spec, |entry| {
            if first_tmpfiles_span.is_some() {
                return;
            }
            let cls = classifier.classify(entry);
            if cls.kind_hints.is_tmpfiles_conf {
                first_tmpfiles_span = Some(cls.span());
            }
        });
        let Some(span) = first_tmpfiles_span else {
            return;
        };
        let mut called = false;
        for_each_scriptlet(spec, |s| {
            if called {
                return;
            }
            for line in &s.body.lines {
                if line_references_any_macro(line, self.policy.tmpfiles_create_macros) {
                    called = true;
                    return;
                }
            }
        });
        if called {
            return;
        }
        let Some(&suggestion) = self.policy.tmpfiles_create_macros.first() else {
            return;
        };
        self.diagnostics.push(Diagnostic::new(
            &METADATA,
            Severity::Warn,
            format!(
                "tmpfiles.d drop-in shipped in `%files` but no scriptlet calls \
                 `%{suggestion}` to materialise the entries"
            ),
            span,
        ));
    }
}

impl Lint for TmpfilesWithoutCreate {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn applies_to_profile(&self, profile: &Profile) -> bool {
        !PolicyRegistry::for_profile(profile)
            .tmpfiles_create_macros
            .is_empty()
    }
    fn set_profile(&mut self, profile: &Profile) {
        self.profile = profile.clone();
        self.policy = PolicyRegistry::for_profile(profile);
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
        for (n, b) in [("_prefix", "/usr"), ("_tmpfilesdir", "/usr/lib/tmpfiles.d")] {
            p.macros
                .insert(n, MacroEntry::literal(b, Provenance::Override));
        }
        p
    }

    fn run(src: &str, profile: &Profile) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = TmpfilesWithoutCreate::new();
        lint.set_profile(profile);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_tmpfiles_without_create() {
        let src = "Name: x\n%files\n/usr/lib/tmpfiles.d/foo.conf\n";
        let diags = run(src, &fedora_profile());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM347");
    }

    #[test]
    fn silent_when_tmpfiles_create_called() {
        let src = "Name: x\n%files\n/usr/lib/tmpfiles.d/foo.conf\n\
%post\n%tmpfiles_create foo.conf\n";
        assert!(run(src, &fedora_profile()).is_empty());
    }

    #[test]
    fn silent_when_no_tmpfiles_in_files() {
        let src = "Name: x\n%files\n/usr/bin/foo\n";
        assert!(run(src, &fedora_profile()).is_empty());
    }

    #[test]
    fn silent_on_generic_profile() {
        let src = "Name: x\n%files\n/usr/lib/tmpfiles.d/foo.conf\n";
        let mut p = Profile::default();
        for (n, b) in [("_prefix", "/usr"), ("_tmpfilesdir", "/usr/lib/tmpfiles.d")] {
            p.macros
                .insert(n, MacroEntry::literal(b, Provenance::Override));
        }
        assert!(run(src, &p).is_empty());
    }
}
