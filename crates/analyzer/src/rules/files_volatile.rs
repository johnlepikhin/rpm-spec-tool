//! RPM369 `var-run-var-lock-not-ghost` — files under `/var/run`,
//! `/run`, or `/var/lock` are listed without `%ghost`.
//!
//! On modern systems `/var/run` and `/var/lock` are tmpfs mounts (or
//! symlinks to `/run`), so files there exist only at runtime. Packaging
//! them as concrete files breaks the install (the destination is
//! volatile) and conflicts with whatever process creates them at
//! startup. The correct idiom is `%ghost` plus a `tmpfiles.d` entry
//! that recreates the path on every boot.

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::files::{FilesClassifier, for_each_files_entry};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;
use rpm_spec_profile::Profile;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM369",
    name: "var-run-var-lock-not-ghost",
    description: "A file under `/var/run`, `/run`, or `/var/lock` is listed without `%ghost`. \
                  Those directories are volatile (tmpfs); package the entry as `%ghost` and \
                  recreate it with `tmpfiles.d`.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

#[derive(Debug, Default)]
pub struct VarRunVarLockNotGhost {
    diagnostics: Vec<Diagnostic>,
    profile: Profile,
}

impl VarRunVarLockNotGhost {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for VarRunVarLockNotGhost {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let classifier = FilesClassifier::new(&self.profile);
        for_each_files_entry(spec, |entry| {
            let cls = classifier.classify(entry);
            if !(cls.kind_hints.under_var_run || cls.kind_hints.under_var_lock) {
                return;
            }
            if cls.directives.is_ghost {
                return;
            }
            let path = cls.resolved_path.as_deref().unwrap_or("");
            self.diagnostics.push(Diagnostic::new(
                &METADATA,
                Severity::Warn,
                format!(
                    "`{path}` lives on a volatile filesystem (`/var/run` / `/run` / `/var/lock`); \
                     mark it `%ghost` and recreate with `tmpfiles.d`"
                ),
                cls.span(),
            ));
        });
    }
}

impl Lint for VarRunVarLockNotGhost {
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

    fn run(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = VarRunVarLockNotGhost::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_var_run_file_not_ghost() {
        let src = "Name: x\n%files\n/var/run/foo.pid\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM369");
    }

    #[test]
    fn flags_run_file_not_ghost() {
        let src = "Name: x\n%files\n/run/foo.sock\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn flags_var_lock_file() {
        let src = "Name: x\n%files\n/var/lock/foo\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn silent_for_ghost_var_run() {
        let src = "Name: x\n%files\n%ghost /var/run/foo.pid\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_regular_paths() {
        let src = "Name: x\n%files\n/var/lib/foo/data\n";
        assert!(run(src).is_empty());
    }
}
