//! RPM366 `duplicate-files-in-files-sections` — the same normalised
//! path is listed twice, either within one `%files` section or across
//! `%files` blocks of different (sub)packages.
//!
//! Within one package, duplicates are dead lines; rpmbuild keeps the
//! last directive. Across subpackages they trigger genuine "file
//! conflicts" at install time — two RPMs claiming ownership of the
//! same path will fail to coexist.
//!
//! The check normalises away `%dir` (own a directory, not its contents)
//! and `%ghost` (claim ownership without packaging), since both legally
//! co-occur with a regular `%files` listing of contents under the same
//! prefix. Globs (`%{_datadir}/*`) are not unfolded — duplicates of
//! exact paths against a glob remain silent (RPM368 handles the broader
//! "glob ate too much" smell).

use std::collections::HashMap;

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::files::{FilesClassifier, for_each_files_entry_with_subpkg, pkg_name_for};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::package_name;
use crate::visit::Visit;
use rpm_spec_profile::Profile;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM366",
    name: "duplicate-files-in-files-sections",
    description: "The same normalised path appears in `%files` more than once. Within one \
                  package it is dead packaging; across subpackages it produces a true file \
                  conflict at install time.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

#[derive(Debug, Default)]
pub struct DuplicateFilesInFilesSections {
    diagnostics: Vec<Diagnostic>,
    profile: Profile,
}

impl DuplicateFilesInFilesSections {
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Clone)]
struct Occurrence {
    package: String,
    span: Span,
}

impl<'ast> Visit<'ast> for DuplicateFilesInFilesSections {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let classifier = FilesClassifier::new(&self.profile);
        let main = package_name(spec).map(str::to_owned);

        // Map from canonical path to (first-seen package + span).
        let mut seen: HashMap<String, Occurrence> = HashMap::new();

        for_each_files_entry_with_subpkg(spec, |subpkg, entry| {
            let cls = classifier.classify(entry);
            if cls.directives.is_dir || cls.directives.is_ghost {
                return;
            }
            let Some(path) = cls.resolved_path.as_deref() else {
                return;
            };
            if path.contains('*') || path.contains('?') {
                // Globs are out of scope — see RPM368.
                return;
            }
            let norm = normalize_path(path);
            // `pkg_name_for` returns an empty string when neither the
            // subpkg nor the main name resolves; render that as
            // `<unnamed>` so cross-package collision messages stay
            // readable.
            let mut pkg = pkg_name_for(main.as_deref(), subpkg);
            if pkg.is_empty() {
                pkg = "<unnamed>".into();
            }

            if let Some(prev) = seen.get(&norm) {
                let cross = prev.package != pkg;
                let msg = if cross {
                    format!(
                        "`{norm}` is listed in both `{prev_pkg}` and `{pkg}` — file conflict at \
                         install time",
                        prev_pkg = prev.package,
                    )
                } else {
                    format!("`{norm}` is listed more than once in package `{pkg}`")
                };
                self.diagnostics.push(
                    Diagnostic::new(&METADATA, Severity::Warn, msg, cls.span())
                        .with_label(prev.span, "first listed here"),
                );
            } else {
                seen.insert(
                    norm,
                    Occurrence {
                        package: pkg,
                        span: cls.span(),
                    },
                );
            }
        });
    }
}

fn normalize_path(path: &str) -> String {
    let trimmed = path.trim().trim_end_matches('/');
    trimmed.to_owned()
}

impl Lint for DuplicateFilesInFilesSections {
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

    fn run(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut p = Profile::default();
        for (n, b) in [
            ("_prefix", "/usr"),
            ("_bindir", "/usr/bin"),
            ("_datadir", "/usr/share"),
        ] {
            p.macros
                .insert(n, MacroEntry::literal(b, Provenance::Override));
        }
        let mut lint = DuplicateFilesInFilesSections::new();
        lint.set_profile(&p);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_duplicate_within_one_files_section() {
        let src = "Name: x\n%files\n/usr/bin/foo\n/usr/bin/foo\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM366");
        assert!(diags[0].message.contains("foo"));
    }

    #[test]
    fn flags_cross_subpackage_collision() {
        let src = "Name: x\n\
%package data\n\
Summary: d\n\
%description data\nbody\n\
%files\n/usr/share/x/info\n\
%files data\n/usr/share/x/info\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("file conflict"));
    }

    #[test]
    fn silent_for_distinct_paths() {
        let src = "Name: x\n%files\n/usr/bin/foo\n/usr/bin/bar\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_dir_plus_file_under_it() {
        // `%dir /usr/share/x` and `/usr/share/x/file` are different
        // paths after normalisation; `%dir` is filtered out anyway.
        let src = "Name: x\n%files\n%dir /usr/share/x\n/usr/share/x/file\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_ghost_vs_real_entry() {
        // `%ghost /var/run/x.pid` plus `/var/run/x.pid` is allowed —
        // RPM369 handles the ghost question separately.
        let src = "Name: x\n%files\n%ghost /var/run/x.pid\n/var/run/x.pid\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_glob_entries() {
        let src = "Name: x\n%files\n%{_datadir}/*\n/usr/share/x/file\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn normalises_trailing_slash() {
        let src = "Name: x\n%files\n/usr/share/x\n/usr/share/x/\n";
        assert_eq!(run(src).len(), 1);
    }
}
