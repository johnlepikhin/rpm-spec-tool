//! `make`-flavoured `%install` rules: RPM382, RPM383.
//!
//! - **RPM382 `makeinstall-without-underscore`** — `%makeinstall` is
//!   the legacy macro that hard-codes `prefix=`, `bindir=`, … on the
//!   make invocation. It exists for old auto-tools projects whose
//!   `make install` didn't honour `DESTDIR`. On modern auto-tools /
//!   meson / cmake projects it produces *worse* output than the
//!   simpler `%make_install` (which is just `make install
//!   DESTDIR=%{buildroot}`). Fedora has deprecated `%makeinstall`.
//! - **RPM383 `make-install-missing-destdir`** — calling `make
//!   install` directly without `DESTDIR=%{buildroot}` installs onto
//!   the host. Use `%make_install` (which sets DESTDIR for you) or
//!   spell the variable out explicitly.

use rpm_spec::ast::{BuildScriptKind, Span, SpecFile, Text, TextSegment};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::shell::{CommandUseIndex, ShellToken, for_each_buildscript};
use crate::visit::Visit;

// =====================================================================
// RPM382 makeinstall-without-underscore
// =====================================================================

pub static MAKEINSTALL_METADATA: LintMetadata = LintMetadata {
    id: "RPM382",
    name: "makeinstall-without-underscore",
    description: "`%makeinstall` is the legacy hard-coded form; prefer `%make_install` \
                  (which sets `DESTDIR=%{buildroot}`).",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// `%makeinstall` is the legacy hard-coded form; prefer `%make_install` (which sets `DESTDIR=%{buildroot}`).
///
/// See [`MAKEINSTALL_METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct MakeinstallWithoutUnderscore {
    diagnostics: Vec<Diagnostic>,
}

impl MakeinstallWithoutUnderscore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for MakeinstallWithoutUnderscore {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for_each_buildscript(spec, |kind, body, span| {
            if kind != BuildScriptKind::Install {
                return;
            }
            for line in &body.lines {
                if line_references_makeinstall_macro(line) {
                    self.diagnostics.push(Diagnostic::new(
                        &MAKEINSTALL_METADATA,
                        Severity::Warn,
                        "`%makeinstall` is deprecated — replace with `%make_install`",
                        span,
                    ));
                    // One diagnostic per `%install` section is plenty.
                    return;
                }
            }
        });
    }
}

fn line_references_makeinstall_macro(line: &Text) -> bool {
    line.segments.iter().any(|seg| match seg {
        TextSegment::Macro(m) => m.name == "makeinstall",
        _ => false,
    })
}

impl Lint for MakeinstallWithoutUnderscore {
    fn metadata(&self) -> &'static LintMetadata {
        &MAKEINSTALL_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM383 make-install-missing-destdir
// =====================================================================

pub static MAKE_INSTALL_DESTDIR_METADATA: LintMetadata = LintMetadata {
    id: "RPM383",
    name: "make-install-missing-destdir",
    description: "`make install` in `%install` without `DESTDIR=%{buildroot}` (or \
                  `$RPM_BUILD_ROOT`) installs onto the build host. Use `%make_install`, or \
                  pass `DESTDIR=` explicitly.",
    default_severity: Severity::Deny,
    category: LintCategory::Correctness,
};

/// `make install` in `%install` without `DESTDIR=%{buildroot}` (or `$RPM_BUILD_ROOT`) installs onto the build host. Use `%make_install`, or pass `DESTDIR=` explicitly.
///
/// See [`MAKE_INSTALL_DESTDIR_METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct MakeInstallMissingDestdir {
    diagnostics: Vec<Diagnostic>,
}

impl MakeInstallMissingDestdir {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for MakeInstallMissingDestdir {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let idx = CommandUseIndex::from_spec(spec);
        for use_ in idx.in_buildscript(BuildScriptKind::Install) {
            if use_.name.as_deref() != Some("make") {
                continue;
            }
            // The line must mention `install` as a target. Targets
            // come after make's options; we scan all tokens.
            if !tokens_contain_install_target(&use_.tokens) {
                continue;
            }
            if tokens_contain_destdir(&use_.tokens) {
                continue;
            }
            self.diagnostics.push(Diagnostic::new(
                &MAKE_INSTALL_DESTDIR_METADATA,
                Severity::Deny,
                "`make install` in `%install` without `DESTDIR=` — either use `%make_install` \
                 or pass `DESTDIR=%{buildroot}` explicitly",
                use_.location.section_span(),
            ));
        }
    }
}

fn tokens_contain_install_target(tokens: &[ShellToken]) -> bool {
    tokens
        .iter()
        .skip(1)
        .filter_map(|t| t.literal_str())
        .any(|s| s == "install" || s.ends_with("/install"))
}

fn tokens_contain_destdir(tokens: &[ShellToken]) -> bool {
    tokens.iter().skip(1).any(|t| {
        let lit = t.render_verbatim();
        lit.starts_with("DESTDIR=")
    })
}

impl Lint for MakeInstallMissingDestdir {
    fn metadata(&self) -> &'static LintMetadata {
        &MAKE_INSTALL_DESTDIR_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run_382(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = MakeinstallWithoutUnderscore::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    fn run_383(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = MakeInstallMissingDestdir::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    // ----- RPM382 -----

    #[test]
    fn rpm382_flags_makeinstall() {
        let src = "Name: x\n%install\n%makeinstall\n";
        let diags = run_382(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM382");
    }

    #[test]
    fn rpm382_silent_for_make_install() {
        let src = "Name: x\n%install\n%make_install\n";
        assert!(run_382(src).is_empty());
    }

    #[test]
    fn rpm382_silent_outside_install_section() {
        let src = "Name: x\n%build\n%makeinstall\n";
        assert!(run_382(src).is_empty());
    }

    // ----- RPM383 -----

    #[test]
    fn rpm383_flags_make_install_without_destdir() {
        let src = "Name: x\n%install\nmake install\n";
        let diags = run_383(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM383");
        assert_eq!(diags[0].severity, Severity::Deny);
    }

    #[test]
    fn rpm383_silent_with_destdir() {
        let src = "Name: x\n%install\nmake install DESTDIR=%{buildroot}\n";
        assert!(run_383(src).is_empty());
    }

    #[test]
    fn rpm383_silent_for_other_make_targets() {
        let src = "Name: x\n%install\nmake check\n";
        assert!(run_383(src).is_empty());
    }

    #[test]
    fn rpm383_silent_outside_install_section() {
        let src = "Name: x\n%build\nmake install\n";
        assert!(run_383(src).is_empty());
    }
}
