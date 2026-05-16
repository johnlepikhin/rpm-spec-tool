//! RPM346 `ldconfig-scriptlet-style` — library packages that invoke
//! `/sbin/ldconfig` from a shell-bodied `%post`/`%postun` instead of
//! the canonical `%post -p /sbin/ldconfig` shorthand.
//!
//! On Fedora ≥ 28 / RHEL 8+, `ldconfig` is run automatically by file
//! triggers — manual invocation is redundant. On older targets the
//! short form `%post -p /sbin/ldconfig` avoids spawning a shell just
//! to run one binary.
//!
//! The rule fires when:
//!
//! 1. `%files` contains a versioned shared-library entry
//!    (`libfoo.so.N`), and
//! 2. A scriptlet runs `/sbin/ldconfig` from a shell body (not via
//!    `-p`).

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::files::{FilesClassifier, for_each_files_entry};
use crate::lint::{Lint, LintMetadata};
use crate::shell::for_each_scriptlet;
use crate::visit::Visit;
use rpm_spec_profile::Profile;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM346",
    name: "ldconfig-scriptlet-style",
    description: "Library package runs `/sbin/ldconfig` from a shell-bodied scriptlet; use \
                  the `%post -p /sbin/ldconfig` interpreter shorthand, or drop the call \
                  entirely on file-trigger-aware distros.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct LdconfigScriptletStyle {
    diagnostics: Vec<Diagnostic>,
    profile: Profile,
}

impl LdconfigScriptletStyle {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for LdconfigScriptletStyle {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let classifier = FilesClassifier::new(&self.profile);
        let mut has_versioned_so = false;
        for_each_files_entry(spec, |entry| {
            if has_versioned_so {
                return;
            }
            let cls = classifier.classify(entry);
            // Versioned shared library (libfoo.so.N) — runtime
            // payload that benefits from ldconfig.
            if let Some(path) = cls.resolved_path.as_deref()
                && path_is_versioned_so(path)
            {
                has_versioned_so = true;
            }
        });
        if !has_versioned_so {
            return;
        }
        for_each_scriptlet(spec, |s| {
            // `%post -p /sbin/ldconfig` uses the interpreter slot
            // directly — we don't flag that form.
            if scriptlet_uses_ldconfig_interpreter(s) {
                return;
            }
            for line in &s.body.lines {
                let Some(lit) = line.literal_str() else {
                    continue;
                };
                if line_calls_ldconfig(lit.trim()) {
                    self.diagnostics.push(Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "scriptlet runs `/sbin/ldconfig` from a shell body; use \
                         `%post -p /sbin/ldconfig` (or drop the call on file-trigger-aware \
                         distros)",
                        s.data,
                    ));
                    return;
                }
            }
        });
    }
}

fn path_is_versioned_so(path: &str) -> bool {
    let last = path.rsplit('/').next().unwrap_or("");
    last.contains(".so.")
}

fn scriptlet_uses_ldconfig_interpreter(s: &rpm_spec::ast::Scriptlet<Span>) -> bool {
    let Some(rpm_spec::ast::Interpreter::Path(t)) = &s.interp else {
        return false;
    };
    let Some(lit) = t.literal_str() else {
        return false;
    };
    matches!(lit.trim(), "/sbin/ldconfig" | "/usr/sbin/ldconfig")
}

fn line_calls_ldconfig(trimmed: &str) -> bool {
    // Tolerate `-r /path` and similar flags. The check is intentionally
    // simple: any bare `ldconfig` / `/sbin/ldconfig` / `/usr/sbin/ldconfig`
    // invocation at the start of the line.
    let first_word = trimmed.split_whitespace().next().unwrap_or("");
    matches!(
        first_word,
        "ldconfig" | "/sbin/ldconfig" | "/usr/sbin/ldconfig"
    )
}

impl Lint for LdconfigScriptletStyle {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
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
    use rpm_spec_profile::{MacroEntry, Profile, Provenance};

    fn profile() -> Profile {
        let mut p = Profile::default();
        for (n, b) in [("_prefix", "/usr"), ("_libdir", "/usr/lib64")] {
            p.macros
                .insert(n, MacroEntry::literal(b, Provenance::Override));
        }
        p
    }

    fn run(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = LdconfigScriptletStyle::new();
        lint.set_profile(&profile());
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_shell_ldconfig_for_versioned_so() {
        let src = "Name: x\n%files\n/usr/lib64/libfoo.so.1\n\
%post\n/sbin/ldconfig\nexit 0\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM346");
    }

    #[test]
    fn silent_for_interpreter_form() {
        let src = "Name: x\n%files\n/usr/lib64/libfoo.so.1\n\
%post -p /sbin/ldconfig\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_no_versioned_so() {
        let src = "Name: x\n%files\n/usr/bin/foo\n\
%post\nldconfig\nexit 0\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_bare_ldconfig_command() {
        let src = "Name: x\n%files\n/usr/lib64/libfoo.so.1.2.3\n\
%postun\nldconfig\nexit 0\n";
        assert_eq!(run(src).len(), 1);
    }
}
