//! RPM384 `install-chown-or-owner` — `%install` calls `chown`,
//! `chgrp`, or uses `install -o`/`install -g`.
//!
//! `%install` runs as an unprivileged user under modern build systems
//! (mock, koji, OBS), so `chown root foo` either silently no-ops or
//! errors out depending on capabilities. The correct way to set
//! ownership is in `%files` via `%attr(MODE, OWNER, GROUP)` (or
//! `%defattr`); rpm honours it at install time when the package
//! is unpacked.

use rpm_spec::ast::{BuildScriptKind, Span, SpecFile};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::shell::{CommandUseIndex, ShellToken};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM384",
    name: "install-chown-or-owner",
    description: "`%install` invokes `chown`/`chgrp` or `install -o`/`install -g`. \
                  `%install` runs unprivileged; ownership belongs in `%files` via `%attr(...)`.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct InstallChownOrOwner {
    diagnostics: Vec<Diagnostic>,
}

impl InstallChownOrOwner {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for InstallChownOrOwner {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let idx = CommandUseIndex::from_spec(spec);
        for use_ in idx.in_buildscript(BuildScriptKind::Install) {
            let Some(name) = use_.name.as_deref() else {
                continue;
            };
            match name {
                "chown" | "chgrp" => {
                    self.diagnostics.push(Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        format!(
                            "`{name}` in `%install` — `%install` runs unprivileged; set \
                             ownership in `%files` via `%attr(MODE, OWNER, GROUP)`"
                        ),
                        use_.location.section_span(),
                    ));
                }
                "install" if tokens_use_owner_or_group_flag(&use_.tokens) => {
                    self.diagnostics.push(Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "`install -o`/`install -g` in `%install` — set ownership in \
                         `%files` via `%attr(MODE, OWNER, GROUP)`",
                        use_.location.section_span(),
                    ));
                }
                _ => {}
            }
        }
    }
}

fn tokens_use_owner_or_group_flag(tokens: &[ShellToken]) -> bool {
    tokens
        .iter()
        .skip(1)
        .filter_map(|t| t.literal_str())
        .any(|s| s == "-o" || s == "-g" || s.starts_with("--owner") || s.starts_with("--group"))
}

impl Lint for InstallChownOrOwner {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = InstallChownOrOwner::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_chown_in_install() {
        let src = "Name: x\n%install\nchown root:root %{buildroot}/etc/foo\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM384");
        assert!(diags[0].message.contains("chown"));
    }

    #[test]
    fn flags_chgrp_in_install() {
        let src = "Name: x\n%install\nchgrp adm %{buildroot}/etc/foo\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn flags_install_dash_o() {
        let src = "Name: x\n%install\ninstall -m 0644 -o root foo %{buildroot}/etc/foo\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn flags_install_dash_g() {
        let src = "Name: x\n%install\ninstall -m 0644 -g root foo %{buildroot}/etc/foo\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn silent_for_install_without_owner_flags() {
        let src = "Name: x\n%install\ninstall -m 0644 foo %{buildroot}/etc/foo\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_outside_install_section() {
        let src = "Name: x\n%post\nchown root /etc/foo\n";
        assert!(run(src).is_empty());
    }
}
