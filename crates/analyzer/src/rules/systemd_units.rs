//! Systemd unit packaging policy: RPM343, RPM344.
//!
//! - **RPM343 `systemd-unit-without-helper-macros`** — `%files`
//!   contains a unit file (`.service`/`.socket`/`.timer`/…), but no
//!   scriptlet calls the distro's lifecycle helpers (`%systemd_post`
//!   on Fedora, `%service_add_post` on openSUSE). Without the helpers
//!   the unit is *packaged but not registered*: systemctl doesn't
//!   know about it after install. Family-aware via
//!   [`PolicyRegistry`].
//! - **RPM344 `systemd-unit-under-etc-or-config`** — the unit file
//!   itself is shipped under `/etc/systemd/system` or carries
//!   `%config`. Unit files belong in `%{_unitdir}` (typically
//!   `/usr/lib/systemd/system`) and should not be `%config` — sysadmins
//!   override units by *masking* them or dropping fragments under
//!   `/etc`, never by editing the package's file in place.

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::files::{FilesClassifier, for_each_files_entry};
use crate::lint::{Lint, LintMetadata};
use crate::policy::{PolicyRegistry, line_references_any_macro};
use crate::shell::for_each_scriptlet;
use crate::visit::Visit;
use rpm_spec_profile::Profile;

// =====================================================================
// RPM343 systemd-unit-without-helper-macros
// =====================================================================

pub static UNIT_NO_HELPERS_METADATA: LintMetadata = LintMetadata {
    id: "RPM343",
    name: "systemd-unit-without-helper-macros",
    description: "`%files` ships a systemd unit (`.service`/`.socket`/...), but no scriptlet \
                  invokes the distro's lifecycle helper macros (`%systemd_*` / `%service_*`). \
                  The unit is packaged but not registered.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

#[derive(Debug, Default)]
pub struct SystemdUnitWithoutHelperMacros {
    diagnostics: Vec<Diagnostic>,
    profile: Profile,
    policy: PolicyRegistry,
}

impl SystemdUnitWithoutHelperMacros {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for SystemdUnitWithoutHelperMacros {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        if self.policy.systemd_macros.is_empty() {
            // Generic profile — no policy to enforce.
            return;
        }
        let classifier = FilesClassifier::new(&self.profile);
        let mut first_unit_span: Option<Span> = None;
        for_each_files_entry(spec, |entry| {
            if first_unit_span.is_some() {
                return;
            }
            let cls = classifier.classify(entry);
            if cls.kind_hints.systemd_unit_ext.is_some() {
                first_unit_span = Some(cls.span());
            }
        });
        let Some(unit_span) = first_unit_span else {
            return;
        };

        let mut helper_called = false;
        for_each_scriptlet(spec, |s| {
            if helper_called {
                return;
            }
            for line in &s.body.lines {
                if line_references_any_macro(line, self.policy.systemd_macros) {
                    helper_called = true;
                    return;
                }
            }
        });
        if helper_called {
            return;
        }
        // `is_empty()` above guarantees a first element, but `.first()`
        // keeps the code safe to refactor without reordering the
        // early-return.
        let Some(&suggestion) = self.policy.systemd_macros.first() else {
            return;
        };
        self.diagnostics.push(Diagnostic::new(
            &UNIT_NO_HELPERS_METADATA,
            Severity::Warn,
            format!(
                "systemd unit shipped in `%files` but no scriptlet calls a lifecycle helper \
                 (e.g. `%{suggestion}`)"
            ),
            unit_span,
        ));
    }
}

impl Lint for SystemdUnitWithoutHelperMacros {
    fn metadata(&self) -> &'static LintMetadata {
        &UNIT_NO_HELPERS_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn applies_to_profile(&self, profile: &Profile) -> bool {
        !PolicyRegistry::for_profile(profile)
            .systemd_macros
            .is_empty()
    }
    fn set_profile(&mut self, profile: &Profile) {
        self.profile = profile.clone();
        self.policy = PolicyRegistry::for_profile(profile);
    }
}

// =====================================================================
// RPM344 systemd-unit-under-etc-or-config
// =====================================================================

pub static UNIT_UNDER_ETC_METADATA: LintMetadata = LintMetadata {
    id: "RPM344",
    name: "systemd-unit-under-etc-or-config",
    description: "A systemd unit is installed under `/etc/systemd/system` or carries `%config`. \
                  Unit files belong in `%{_unitdir}` (typically `/usr/lib/systemd/system`) and \
                  should not be `%config`.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

#[derive(Debug, Default)]
pub struct SystemdUnitUnderEtcOrConfig {
    diagnostics: Vec<Diagnostic>,
    profile: Profile,
}

impl SystemdUnitUnderEtcOrConfig {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for SystemdUnitUnderEtcOrConfig {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let classifier = FilesClassifier::new(&self.profile);
        for_each_files_entry(spec, |entry| {
            let cls = classifier.classify(entry);
            let Some(ext) = cls.kind_hints.systemd_unit_ext else {
                return;
            };
            let path = cls.resolved_path.as_deref().unwrap_or("");
            if path.starts_with("/etc/systemd/system/") {
                self.diagnostics.push(Diagnostic::new(
                    &UNIT_UNDER_ETC_METADATA,
                    Severity::Warn,
                    format!(
                        "{ext} unit installed under `/etc/systemd/system/`; package it in \
                         `%{{_unitdir}}` instead"
                    ),
                    cls.span(),
                ));
                return;
            }
            if cls.directives.config.is_some() {
                self.diagnostics.push(Diagnostic::new(
                    &UNIT_UNDER_ETC_METADATA,
                    Severity::Warn,
                    format!(
                        "systemd {ext} unit marked `%config`; units are not user-editable in \
                         place — drop `%config`"
                    ),
                    cls.span(),
                ));
            }
        });
    }
}

impl Lint for SystemdUnitUnderEtcOrConfig {
    fn metadata(&self) -> &'static LintMetadata {
        &UNIT_UNDER_ETC_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_profile(&mut self, profile: &Profile) {
        self.profile = profile.clone();
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
        for (n, b) in [
            ("_prefix", "/usr"),
            ("_unitdir", "/usr/lib/systemd/system"),
            ("_sysconfdir", "/etc"),
        ] {
            p.macros
                .insert(n, MacroEntry::literal(b, Provenance::Override));
        }
        p
    }

    fn opensuse_profile() -> Profile {
        let mut p = Profile::default();
        p.identity.family = Some(Family::Opensuse);
        for (n, b) in [("_prefix", "/usr"), ("_unitdir", "/usr/lib/systemd/system")] {
            p.macros
                .insert(n, MacroEntry::literal(b, Provenance::Override));
        }
        p
    }

    fn generic_profile() -> Profile {
        let mut p = Profile::default();
        for (n, b) in [("_prefix", "/usr"), ("_sysconfdir", "/etc")] {
            p.macros
                .insert(n, MacroEntry::literal(b, Provenance::Override));
        }
        p
    }

    fn run_343(src: &str, profile: &Profile) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = SystemdUnitWithoutHelperMacros::new();
        lint.set_profile(profile);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    fn run_344(src: &str, profile: &Profile) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = SystemdUnitUnderEtcOrConfig::new();
        lint.set_profile(profile);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    // ----- RPM343 -----

    #[test]
    fn rpm343_flags_unit_without_helpers_on_fedora() {
        let src = "Name: x\n%files\n/usr/lib/systemd/system/foo.service\n";
        let diags = run_343(src, &fedora_profile());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM343");
        assert!(diags[0].message.contains("systemd_post"));
    }

    #[test]
    fn rpm343_silent_when_systemd_post_present() {
        let src = "Name: x\n%files\n/usr/lib/systemd/system/foo.service\n\
%post\n%systemd_post foo.service\n";
        assert!(run_343(src, &fedora_profile()).is_empty());
    }

    #[test]
    fn rpm343_silent_when_helper_called_from_preun() {
        // The walker visits every scriptlet phase; a Fedora package
        // that only ships `%preun` with `%systemd_preun` is fine
        // (the lint is "any helper anywhere", not "in %post").
        let src = "Name: x\n%files\n/usr/lib/systemd/system/foo.service\n\
%preun\n%systemd_preun foo.service\n";
        assert!(run_343(src, &fedora_profile()).is_empty());
    }

    #[test]
    fn rpm343_uses_opensuse_helper_name() {
        let src = "Name: x\n%files\n/usr/lib/systemd/system/foo.service\n";
        let diags = run_343(src, &opensuse_profile());
        assert_eq!(diags.len(), 1);
        assert!(
            diags[0].message.contains("service_add_pre")
                || diags[0].message.contains("service_add_post"),
            "got: {}",
            diags[0].message
        );
    }

    #[test]
    fn rpm343_silent_on_generic_profile() {
        let src = "Name: x\n%files\n/usr/lib/systemd/system/foo.service\n";
        assert!(run_343(src, &generic_profile()).is_empty());
    }

    #[test]
    fn rpm343_silent_when_no_unit_in_files() {
        let src = "Name: x\n%files\n/usr/bin/foo\n";
        assert!(run_343(src, &fedora_profile()).is_empty());
    }

    // ----- RPM344 -----

    #[test]
    fn rpm344_flags_unit_under_etc() {
        let src = "Name: x\n%files\n/etc/systemd/system/foo.service\n";
        let diags = run_344(src, &fedora_profile());
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM344");
    }

    #[test]
    fn rpm344_flags_unit_marked_config() {
        let src = "Name: x\n%files\n%config /usr/lib/systemd/system/foo.service\n";
        let diags = run_344(src, &fedora_profile());
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("`%config`"));
    }

    #[test]
    fn rpm344_silent_for_regular_unit() {
        let src = "Name: x\n%files\n/usr/lib/systemd/system/foo.service\n";
        assert!(run_344(src, &fedora_profile()).is_empty());
    }
}
