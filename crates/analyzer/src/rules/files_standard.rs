//! Standard-directory ownership rules: RPM367, RPM368.
//!
//! - **RPM367 `standard-dir-owned`** — the package's `%files` lists a
//!   bare standard directory (e.g. `%{_bindir}`, `/usr/share`) without
//!   a package-specific sub-path. Doing so claims ownership of a
//!   directory every other package also wants to populate.
//! - **RPM368 `broad-files-glob`** — `%{_datadir}/*` /
//!   `%{_libdir}/*` / `%{_bindir}/*` glob entries. They sweep up newly
//!   added files silently between versions and hide ownership errors;
//!   prefer explicit per-file or per-subdir listings.

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::files::{FilesClassifier, for_each_files_entry};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;
use rpm_spec_profile::Profile;

// =====================================================================
// RPM367 standard-dir-owned
// =====================================================================

pub static STANDARD_DIR_METADATA: LintMetadata = LintMetadata {
    id: "RPM367",
    name: "standard-dir-owned",
    description: "A `%files` entry owns a standard directory (e.g. `%{_bindir}`, `%{_datadir}`) \
                  outright. Standard directories belong to `filesystem` (or the distro \
                  equivalent); list a package-specific sub-path instead.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

/// A `%files` entry owns a standard directory (e.g. `%{_bindir}`, `%{_datadir}`) outright. Standard directories belong to `filesystem` (or the distro equivalent); list a package-specific sub-path instead.
///
/// See [`STANDARD_DIR_METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct StandardDirOwned {
    diagnostics: Vec<Diagnostic>,
    profile: Profile,
}

impl StandardDirOwned {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for StandardDirOwned {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let classifier = FilesClassifier::new(&self.profile);
        for_each_files_entry(spec, |entry| {
            let cls = classifier.classify(entry);
            let Some(macro_name) = cls.kind_hints.standard_dir_macro else {
                return;
            };
            let path = cls.resolved_path.as_deref().unwrap_or("");
            self.diagnostics.push(Diagnostic::new(
                &STANDARD_DIR_METADATA,
                Severity::Warn,
                format!(
                    "package owns standard directory `{path}` (`%{{{macro_name}}}`); other \
                     packages share this directory — list a package-specific sub-path instead"
                ),
                cls.span(),
            ));
        });
    }
}

impl Lint for StandardDirOwned {
    fn metadata(&self) -> &'static LintMetadata {
        &STANDARD_DIR_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_profile(&mut self, profile: &Profile) {
        self.profile = profile.clone();
    }
}

// =====================================================================
// RPM368 broad-files-glob
// =====================================================================

pub static BROAD_GLOB_METADATA: LintMetadata = LintMetadata {
    id: "RPM368",
    name: "broad-files-glob",
    description: "A `%files` entry uses a broad glob (`%{_datadir}/*`, `%{_libdir}/*`, …). \
                  Such globs hide newly added or misnamed files between upstream releases — \
                  list a package-specific subdirectory instead.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

/// A `%files` entry uses a broad glob (`%{_datadir}/*`, `%{_libdir}/*`, …). Such globs hide newly added or misnamed files between upstream releases — list a package-specific subdirectory instead.
///
/// See [`BROAD_GLOB_METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct BroadFilesGlob {
    diagnostics: Vec<Diagnostic>,
    profile: Profile,
}

impl BroadFilesGlob {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for BroadFilesGlob {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let classifier = FilesClassifier::new(&self.profile);
        for_each_files_entry(spec, |entry| {
            let cls = classifier.classify(entry);
            let Some(macro_name) = cls.kind_hints.broad_glob_for else {
                return;
            };
            self.diagnostics.push(Diagnostic::new(
                &BROAD_GLOB_METADATA,
                Severity::Warn,
                format!(
                    "broad glob `%{{{macro_name}}}/*` claims everything under a shared \
                     directory; list package-specific sub-paths to keep ownership explicit"
                ),
                cls.span(),
            ));
        });
    }
}

impl Lint for BroadFilesGlob {
    fn metadata(&self) -> &'static LintMetadata {
        &BROAD_GLOB_METADATA
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

    fn fedora_profile() -> Profile {
        let mut p = Profile::default();
        for (n, b) in [
            ("_prefix", "/usr"),
            ("_bindir", "/usr/bin"),
            ("_libdir", "/usr/lib64"),
            ("_datadir", "/usr/share"),
            ("_sysconfdir", "/etc"),
        ] {
            p.macros
                .insert(n, MacroEntry::literal(b, Provenance::Override));
        }
        p
    }

    fn run_367(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = StandardDirOwned::new();
        lint.set_profile(&fedora_profile());
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    fn run_368(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = BroadFilesGlob::new();
        lint.set_profile(&fedora_profile());
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    // ----- RPM367 -----

    #[test]
    fn rpm367_flags_bare_bindir() {
        let src = "Name: x\n%files\n%{_bindir}\n";
        let diags = run_367(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM367");
    }

    #[test]
    fn rpm367_flags_bare_datadir() {
        let src = "Name: x\n%files\n%{_datadir}\n";
        assert_eq!(run_367(src).len(), 1);
    }

    #[test]
    fn rpm367_silent_for_subdir() {
        let src = "Name: x\n%files\n%{_bindir}/foo\n";
        assert!(run_367(src).is_empty());
    }

    #[test]
    fn rpm367_silent_for_glob() {
        // The glob form is RPM368's responsibility — RPM367 keys on
        // exact-match standard dirs only.
        let src = "Name: x\n%files\n%{_bindir}/*\n";
        assert!(run_367(src).is_empty());
    }

    // ----- RPM368 -----

    #[test]
    fn rpm368_flags_datadir_star() {
        let src = "Name: x\n%files\n%{_datadir}/*\n";
        let diags = run_368(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM368");
    }

    #[test]
    fn rpm368_flags_libdir_star() {
        let src = "Name: x\n%files\n%{_libdir}/*\n";
        assert_eq!(run_368(src).len(), 1);
    }

    #[test]
    fn rpm368_silent_for_specific_subdir() {
        let src = "Name: x\n%files\n%{_datadir}/foo/*\n";
        assert!(run_368(src).is_empty());
    }

    #[test]
    fn rpm368_silent_for_bare_macro_no_glob() {
        // Bare `%{_datadir}` is RPM367, not RPM368.
        let src = "Name: x\n%files\n%{_datadir}\n";
        assert!(run_368(src).is_empty());
    }
}
