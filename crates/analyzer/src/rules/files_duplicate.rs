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
use crate::rules::util::{package_name, render_text_with_macros};
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

/// The same normalised path appears in `%files` more than once. Within one package it is dead packaging; across subpackages it produces a true file conflict at install time.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
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
            // `%license` / `%doc` with a *relative* path get installed
            // under a per-package directory (`%{_licensedir}/%{name}/…`
            // or `%{_docdir}/%{name}/…`), so two subpackages listing
            // the same `gpl.txt` are NOT a file conflict — they end up
            // in different package-scoped directories. Skip these to
            // avoid the texlive-base flood (100+ subpackages each with
            // `%license gpl.txt`). An absolute path under `%license`
            // is still considered (less common, but the per-package
            // remapping no longer applies).
            if (cls.directives.is_license || cls.directives.is_doc) && !path.starts_with('/') {
                return;
            }
            let norm = normalize_path(path);
            // `pkg_name_for` returns an empty string when neither the
            // subpkg nor the main name resolves. Falling back to
            // `<unnamed>` collapses every macro-templated subpackage
            // (`%files -n %{shortname}-FOO`) into a single bucket and
            // produces a flood of false-positive "duplicate" diagnostics
            // (e.g. 100+ subpackages each with `%license gpl.txt`).
            // Instead, prefer the literal source-text rendering of the
            // subpkg ref as the bucket key. Two textually-identical
            // templates DO collide (correctly flagged); two textually
            // different ones do not.
            let mut pkg = pkg_name_for(main.as_deref(), subpkg);
            if pkg.is_empty() {
                pkg = render_subpkg_ref(subpkg).unwrap_or_else(|| "<unnamed>".into());
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

/// Render a [`SubpkgRef`] back to its source-text form, preserving
/// macro references as `%foo` / `%{foo}`. Used as a stable bucket key
/// when [`pkg_name_for`] cannot fully resolve the subpackage name
/// (typically when the header contains an unresolved macro like
/// `%{shortname}`). Returns `None` for refs that don't resolve to a
/// non-empty string.
fn render_subpkg_ref(subpkg: Option<&rpm_spec::ast::SubpkgRef>) -> Option<String> {
    use rpm_spec::ast::SubpkgRef;
    let text = match subpkg? {
        SubpkgRef::Relative(t) | SubpkgRef::Absolute(t) => t,
        _ => return None,
    };
    let rendered = render_text_with_macros(text);
    let trimmed = rendered.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
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

    #[test]
    fn silent_for_macro_templated_subpackages_with_same_license() {
        // Real repro from `texlive-base.spec`: 100+ `%files -n
        // %{shortname}-FOO` blocks each carry `%license gpl.txt`.
        // Because `%{shortname}` doesn't expand at analysis time,
        // `pkg_name_for` returns the empty string for both blocks; the
        // old code then bucketed them together under `<unnamed>` and
        // emitted a flood of false-positive cross-package duplicates.
        // Two textually-distinct subpkg templates must NOT collide.
        let src = "%global shortname acme\n\
                   Name: %{shortname}-base\n\
                   %files -n %{shortname}-a2ping\n\
                   %license gpl.txt\n\
                   %files -n %{shortname}-accfonts\n\
                   %license gpl.txt\n";
        let diags = run(src);
        assert!(
            diags.is_empty(),
            "textually-distinct macro-templated subpackages must not collide: {diags:?}"
        );
    }

    #[test]
    fn fires_for_identical_macro_templated_subpackages() {
        // The flip side of the FP fix: two `%files` blocks with the
        // *literally identical* subpkg-ref template AND the same file
        // ARE a real cross-section duplicate and must still trigger.
        // We use a bare suffix here (`%files foo`) rather than `-n` so
        // both sections resolve to the same concrete package name; the
        // intent is to confirm the bucketing still groups true
        // duplicates correctly.
        let src = "Name: x\n\
                   %package foo\nSummary: f\n%description foo\nbody\n\
                   %files foo\n/usr/share/x/info\n\
                   %files foo\n/usr/share/x/info\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }
}
