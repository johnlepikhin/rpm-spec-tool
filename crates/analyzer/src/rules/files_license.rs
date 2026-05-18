//! RPM363 `license-file-marked-doc` — files whose basename matches a
//! known license filename (`LICENSE`, `COPYING`, `NOTICE`, `AUTHORS`,
//! …) are listed under `%doc` instead of `%license`.
//!
//! RPM 4.11 introduced the `%license` directive specifically to
//! separate legal metadata from runtime documentation: `%license`
//! files survive `rpm --excludedocs` and end up under
//! `%{_defaultlicensedir}` for downstream automation (compliance
//! scanners, distro-wide license aggregators). Marking a license file
//! as `%doc` loses that distinction.
//!
//! Filenames recognised as licenses (case-insensitive prefix on the
//! basename): `LICENSE`, `LICENCE`, `COPYING`, `COPYRIGHT`, `NOTICE`,
//! `AUTHORS`, `MIT-LICENSE`, `LEGAL`. The match is *prefix* so e.g.
//! `LICENSE.txt`, `LICENSE-MIT`, `COPYING.LIB` all qualify.

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::files::{FilesClassifier, for_each_files_entry};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;
use rpm_spec_profile::Profile;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM363",
    name: "license-file-marked-doc",
    description: "A file whose basename looks like a license (`LICENSE`, `COPYING`, `NOTICE`, …) \
                  is marked `%doc` instead of `%license`. `%license` survives \
                  `rpm --excludedocs` and is recognised by compliance tooling.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

/// A file whose basename looks like a license (`LICENSE`, `COPYING`, `NOTICE`, …) is marked `%doc` instead of `%license`. `%license` survives `rpm --excludedocs` and is recognised by compliance tooling.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct LicenseFileMarkedDoc {
    diagnostics: Vec<Diagnostic>,
    profile: Profile,
}

impl LicenseFileMarkedDoc {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for LicenseFileMarkedDoc {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let classifier = FilesClassifier::new(&self.profile);
        for_each_files_entry(spec, |entry| {
            let cls = classifier.classify(entry);
            if cls.directives.is_license {
                return;
            }
            // The path may be unresolvable (macro-only). Fall back to
            // the raw literal-segment text when needed; the filename
            // check is purely string-based.
            let path = match cls.resolved_path.as_deref() {
                Some(p) => p.to_owned(),
                None => fallback_literal(entry).unwrap_or_default(),
            };
            // Only flag when the file is explicitly under `%doc`; a
            // raw entry without any directive is just packaging,
            // which a separate rule should catch.
            if !cls.directives.is_doc {
                return;
            }
            // A single `%doc` directive may list multiple files on one
            // line, e.g. `%doc LICENSE NOTICE`. The classifier reports
            // the whole line as one entry with a concatenated
            // `resolved_path`. Split on whitespace and check each
            // component independently so the diagnostic names the real
            // filename rather than the whole concatenated string.
            for component in path.split_ascii_whitespace() {
                let basename = component.rsplit('/').next().unwrap_or("");
                if !looks_like_license(basename) {
                    continue;
                }
                self.diagnostics.push(Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    format!(
                        "license file `{basename}` is marked `%doc`; switch to `%license` so it \
                         survives `--excludedocs` and lands in `%{{_defaultlicensedir}}`"
                    ),
                    cls.span(),
                ));
            }
        });
    }
}

impl Lint for LicenseFileMarkedDoc {
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

const LICENSE_PREFIXES: &[&str] = &[
    "license",
    "licence",
    "copying",
    "copyright",
    "notice",
    "authors",
    "mit-license",
    "legal",
    "unlicense",
    "gpl",
    "lgpl",
    "agpl",
    "apache-license",
];

fn looks_like_license(basename: &str) -> bool {
    let lower = basename.to_ascii_lowercase();
    LICENSE_PREFIXES.iter().any(|prefix| {
        if let Some(rest) = lower.strip_prefix(prefix) {
            // Either an exact match or followed by a non-alphanumeric
            // separator (`LICENSE.txt`, `LICENSE-MIT`). Avoid matching
            // `LICENSEFAKE` by requiring a boundary.
            rest.is_empty()
                || rest
                    .chars()
                    .next()
                    .is_some_and(|c| !c.is_ascii_alphanumeric())
        } else {
            false
        }
    })
}

fn fallback_literal(entry: &rpm_spec::ast::FileEntry<Span>) -> Option<String> {
    let path = entry.path.as_ref()?;
    let mut out = String::new();
    for seg in &path.path.segments {
        if let rpm_spec::ast::TextSegment::Literal(s) = seg {
            out.push_str(s);
        }
    }
    if out.trim().is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::test_support::run_lint;

    fn run(src: &str) -> Vec<Diagnostic> {
        run_lint::<LicenseFileMarkedDoc>(src)
    }

    #[test]
    fn flags_license_marked_doc() {
        let src = "Name: x\n%files\n%doc LICENSE\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM363");
        assert!(diags[0].message.contains("LICENSE"));
    }

    #[test]
    fn flags_copying_marked_doc() {
        let src = "Name: x\n%files\n%doc COPYING.LIB\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn flags_notice_doc() {
        let src = "Name: x\n%files\n%doc NOTICE\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn silent_for_license_marked_license() {
        let src = "Name: x\n%files\n%license LICENSE\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_readme_marked_doc() {
        // README isn't a license file; %doc is correct.
        let src = "Name: x\n%files\n%doc README.md\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_undirected_license_file() {
        // A bare `LICENSE` line (no %doc, no %license) is a separate
        // packaging smell — RPM363 only fires when the user explicitly
        // chose %doc.
        let src = "Name: x\n%files\nLICENSE\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn boundary_match_rejects_licensefake() {
        // `LICENSEFAKE` has the prefix `LICENSE` but no separator,
        // so it should not be flagged.
        let src = "Name: x\n%files\n%doc LICENSEFAKE\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_each_license_basename_in_multi_name_doc() {
        // `%doc LICENSE NOTICE` is exposed as a single FileEntry with a
        // concatenated resolved path. Both names are license-like, so
        // we emit one diagnostic per matching component, and each
        // diagnostic must reference the bare filename — not the
        // concatenated `"LICENSE NOTICE"` string the user can't act on.
        let src = "Name: x\n%files\n%doc LICENSE NOTICE\n";
        let diags = run(src);
        assert_eq!(diags.len(), 2, "{diags:?}");
        assert!(diags.iter().all(|d| d.lint_id == "RPM363"));
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("`LICENSE`") && !d.message.contains("LICENSE NOTICE")),
            "expected a diag naming bare `LICENSE`, got {diags:?}"
        );
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("`NOTICE`") && !d.message.contains("LICENSE NOTICE")),
            "expected a diag naming bare `NOTICE`, got {diags:?}"
        );
    }

    #[test]
    fn silent_for_multi_name_doc_with_no_license_basename() {
        // Neither README nor CHANGELOG matches a license-file pattern,
        // so a multi-name `%doc` line containing only these must not
        // raise RPM363 — even though the classifier hands us the
        // concatenated path `"README CHANGELOG"`.
        let src = "Name: x\n%files\n%doc README CHANGELOG\n";
        assert!(run(src).is_empty());
    }
}
