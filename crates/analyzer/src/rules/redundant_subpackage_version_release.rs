//! RPM552 `redundant-subpackage-version-release` — flag subpackage
//! preambles that explicitly set `Version:` or `Release:` to the
//! main package's value (literal match or `%{version}` / `%{release}`).
//!
//! Subpackages inherit `Version` / `Release` from the main package by
//! default — the explicit duplicate is dead code and drifts during
//! version bumps.

use rpm_spec::ast::{
    PreambleContent, PreambleItem, Section, Span, SpecFile, SpecItem, Tag, TagValue, Text,
    TextSegment,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::collect_top_level_preamble;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM552",
    name: "redundant-subpackage-version-release",
    description: "Subpackage preamble explicitly sets `Version:` or `Release:` to the main \
                  package's value — drop the duplicate; subpackages inherit by default.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Subpackage preamble explicitly sets `Version:` or `Release:` to the main package's value — drop the duplicate; subpackages inherit by default.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct RedundantSubpackageVersionRelease {
    diagnostics: Vec<Diagnostic>,
}

impl RedundantSubpackageVersionRelease {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for RedundantSubpackageVersionRelease {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let main_version = main_text_for(spec, |t| matches!(t, Tag::Version));
        let main_release = main_text_for(spec, |t| matches!(t, Tag::Release));

        for item in &spec.items {
            let SpecItem::Section(boxed) = item else {
                continue;
            };
            let Section::Package { content, .. } = boxed.as_ref() else {
                continue;
            };
            for sub in content {
                let PreambleContent::Item(p) = sub else {
                    continue;
                };
                self.check_item(p, main_version.as_deref(), main_release.as_deref());
            }
        }
    }
}

impl RedundantSubpackageVersionRelease {
    fn check_item(
        &mut self,
        item: &PreambleItem<Span>,
        main_version: Option<&str>,
        main_release: Option<&str>,
    ) {
        let (label, main_value, ref_macro) = match item.tag {
            Tag::Version => ("Version", main_version, "%{version}"),
            Tag::Release => ("Release", main_release, "%{release}"),
            _ => return,
        };
        let TagValue::Text(t) = &item.value else {
            return;
        };
        // Case A: explicit `%{version}` / `%{release}` macro reference.
        if is_single_macro_ref(t, &label.to_ascii_lowercase()) {
            self.diagnostics
                .push(self.make_diag(item, label, ref_macro));
            return;
        }
        // Case B: literal value matching the main tag.
        if let Some(main) = main_value
            && let Some(lit) = t.literal_str()
            && lit.trim() == main.trim()
        {
            self.diagnostics
                .push(self.make_diag(item, label, "the same literal as the main tag"));
        }
    }

    fn make_diag(&self, item: &PreambleItem<Span>, label: &str, hint: &str) -> Diagnostic {
        Diagnostic::new(
            &METADATA,
            Severity::Warn,
            format!(
                "subpackage `{label}` matches the main package's value ({hint}) — drop the \
                 redundant line"
            ),
            item.data,
        )
        .with_suggestion(Suggestion::new(
            format!("remove the explicit `{label}:` from this subpackage preamble"),
            Vec::new(),
            Applicability::Manual,
        ))
    }
}

fn main_text_for<F>(spec: &SpecFile<Span>, matcher: F) -> Option<String>
where
    F: Fn(&Tag) -> bool,
{
    for item in collect_top_level_preamble(spec) {
        if matcher(&item.tag)
            && let TagValue::Text(t) = &item.value
            && let Some(s) = t.literal_str()
        {
            return Some(s.trim().to_owned());
        }
    }
    None
}

fn is_single_macro_ref(t: &Text, name: &str) -> bool {
    use rpm_spec::ast::{ConditionalMacro, MacroKind};
    let mut macros = Vec::new();
    for seg in &t.segments {
        match seg {
            TextSegment::Literal(s) => {
                if !s.trim().is_empty() {
                    return false;
                }
            }
            TextSegment::Macro(m) => macros.push(m),
            _ => return false,
        }
    }
    if macros.len() != 1 {
        return false;
    }
    let m = macros[0];
    matches!(m.kind, MacroKind::Plain | MacroKind::Braced)
        && matches!(m.conditional, ConditionalMacro::None)
        && m.args.is_empty()
        && m.with_value.is_none()
        && m.name == name
}

impl Lint for RedundantSubpackageVersionRelease {
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
    use crate::rules::test_support::run_lint;

    fn run(src: &str) -> Vec<Diagnostic> {
        run_lint::<RedundantSubpackageVersionRelease>(src)
    }

    #[test]
    fn flags_redundant_version_macro_in_subpkg() {
        let src = "Name: x\nVersion: 1.0\nRelease: 1\n\
%package devel\n\
Summary: dev\n\
Version: %{version}\n\
%description devel\nbody\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM552");
    }

    #[test]
    fn flags_redundant_release_literal_in_subpkg() {
        let src = "Name: x\nVersion: 1.0\nRelease: 5\n\
%package devel\n\
Summary: dev\n\
Release: 5\n\
%description devel\nbody\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_different_version_value() {
        let src = "Name: x\nVersion: 1.0\n\
%package devel\n\
Summary: dev\n\
Version: 2.0\n\
%description devel\nbody\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_no_explicit_subpkg_version() {
        let src = "Name: x\nVersion: 1.0\n\
%package devel\n\
Summary: dev\n\
%description devel\nbody\n";
        assert!(run(src).is_empty());
    }
}
