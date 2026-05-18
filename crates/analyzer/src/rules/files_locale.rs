//! RPM365 `locale-file-not-lang` — `.mo` translations under
//! `/usr/share/locale/` (or `/usr/lib/locale/`) are listed manually
//! without `%lang(...)`.
//!
//! When a translation file is owned by the package without `%lang`,
//! `rpm --installlangs` cannot filter it out, defeating the
//! locale-trimming knob distributions rely on for image size. Two
//! correct forms exist:
//!
//! - `%lang(ru) /usr/share/locale/ru/LC_MESSAGES/foo.mo` — explicit
//!   per-locale entry.
//! - `%find_lang foo` in `%install` plus `%files -f foo.lang` — the
//!   canonical Fedora pattern that auto-generates the list.
//!
//! When a `%files` section reads its content from `-f <name>.lang` we
//! cannot statically prove the locales are owned correctly; the
//! presence of any `-f` argument silences the rule for that section
//! as a conservative bail-out.

use rpm_spec::ast::{FilesContent, Span, SpecFile};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::files::{FilesClassifier, for_each_files_section};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;
use rpm_spec_profile::Profile;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM365",
    name: "locale-file-not-lang",
    description: "A `.mo` translation under `/usr/share/locale/` is listed manually without \
                  `%lang(...)`. Prefer `%find_lang` in `%install` + `%files -f <name>.lang`, \
                  or annotate the entry with `%lang(<code>)`.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

/// A `.mo` translation under `/usr/share/locale/` is listed manually without `%lang(...)`. Prefer `%find_lang` in `%install` + `%files -f <name>.lang`, or annotate the entry with `%lang(<code>)`.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct LocaleFileNotLang {
    diagnostics: Vec<Diagnostic>,
    profile: Profile,
}

impl LocaleFileNotLang {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for LocaleFileNotLang {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let classifier = FilesClassifier::new(&self.profile);
        let diagnostics = &mut self.diagnostics;
        for_each_files_section(spec, |sec| {
            // `%files -f some.lang` — the lang file is auto-generated;
            // we can't prove individual entries are or aren't correctly
            // tagged, so the rule stays quiet for the section. Mirrors
            // how Fedora packaging guidelines treat the `%find_lang`
            // flow.
            if !sec.file_lists.is_empty() {
                return;
            }
            scan_content(&classifier, sec.content, diagnostics);
        });
    }
}

/// Free function so the recursion doesn't need a `&mut self` borrow
/// (which would conflict with the `FilesClassifier` already borrowing
/// `&self.profile` at the call site).
fn scan_content(
    classifier: &FilesClassifier<'_>,
    items: &[FilesContent<Span>],
    out: &mut Vec<Diagnostic>,
) {
    for item in items {
        match item {
            FilesContent::Entry(e) => {
                let cls = classifier.classify(e);
                if !cls.kind_hints.is_locale_mo {
                    continue;
                }
                if cls.directives.has_lang {
                    continue;
                }
                let path = cls.resolved_path.as_deref().unwrap_or("");
                out.push(Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    format!(
                        "`{path}` is a locale translation without `%lang(...)`; use \
                         `%find_lang` + `%files -f <name>.lang`, or annotate per-locale \
                         entries"
                    ),
                    cls.span(),
                ));
            }
            FilesContent::Conditional(c) => {
                for branch in &c.branches {
                    scan_content(classifier, &branch.body, out);
                }
                if let Some(els) = &c.otherwise {
                    scan_content(classifier, els, out);
                }
            }
            _ => {}
        }
    }
}

impl Lint for LocaleFileNotLang {
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
        for (n, b) in [("_prefix", "/usr"), ("_datadir", "/usr/share")] {
            p.macros
                .insert(n, MacroEntry::literal(b, Provenance::Override));
        }
        let mut lint = LocaleFileNotLang::new();
        lint.set_profile(&p);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_mo_without_lang() {
        let src = "Name: x\n%files\n/usr/share/locale/ru/LC_MESSAGES/foo.mo\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM365");
    }

    #[test]
    fn silent_for_mo_with_lang() {
        let src = "Name: x\n%files\n%lang(ru) /usr/share/locale/ru/LC_MESSAGES/foo.mo\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_files_uses_find_lang() {
        let src = "Name: x\n%files -f foo.lang\n/usr/share/locale/ru/LC_MESSAGES/foo.mo\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_non_locale_path() {
        let src = "Name: x\n%files\n/usr/share/foo/data.dat\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_multiple_locales_separately() {
        let src = "Name: x\n%files\n\
/usr/share/locale/ru/LC_MESSAGES/foo.mo\n\
/usr/share/locale/de/LC_MESSAGES/foo.mo\n";
        assert_eq!(run(src).len(), 2);
    }
}
