//! RPM059 `description-shorter-than-summary` — the package
//! `%description` is meant to expand on the one-line `Summary:`. When
//! it ends up shorter than the Summary itself it's almost always a
//! placeholder ("TODO", "Description goes here.") that slipped past
//! review.
//!
//! ## Known limitation
//!
//! Only the **main package** is checked. Subpackage descriptions live
//! in `%description -n foo` sections whose pairing with the matching
//! `%package -n foo` preamble requires extra plumbing; that will land
//! when we generalise `iter_packages` to expose descriptions.

use rpm_spec::ast::{Section, Span, SpecFile, Tag, TagValue, TextSegment};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM059",
    name: "description-shorter-than-summary",
    description: "Main package %description is shorter than its Summary — looks like a placeholder. \
         Subpackage descriptions are not checked yet.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct DescriptionShorterThanSummary {
    diagnostics: Vec<Diagnostic>,
}

impl DescriptionShorterThanSummary {
    pub fn new() -> Self {
        Self::default()
    }
}

fn summary_text_of(spec: &SpecFile<Span>) -> Option<String> {
    use rpm_spec::ast::SpecItem;
    for item in &spec.items {
        if let SpecItem::Preamble(p) = item
            && matches!(p.tag, Tag::Summary)
            && let TagValue::Text(t) = &p.value
            && let Some(s) = t.literal_str()
        {
            return Some(s.trim().to_owned());
        }
    }
    None
}

fn description_text_len(node: &Section<Span>) -> usize {
    let Section::Description { body, .. } = node else {
        return 0;
    };
    body.lines
        .iter()
        .flat_map(|line| line.segments.iter())
        .map(|seg| match seg {
            TextSegment::Literal(s) => s.trim().chars().count(),
            // Macros are opaque; treat them as having at least one
            // character of "content" so a description that's just a
            // single `%{?...}` isn't claimed to be empty.
            TextSegment::Macro(_) => 1,
            _ => 0,
        })
        .sum()
}

impl<'ast> Visit<'ast> for DescriptionShorterThanSummary {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        // Capture the main Summary up front so we can compare each main
        // %description against it; `walk_spec` then handles dispatch.
        let summary_len = summary_text_of(spec)
            .map(|s| s.chars().count())
            .unwrap_or(0);
        if summary_len == 0 {
            return;
        }
        for item in &spec.items {
            let rpm_spec::ast::SpecItem::Section(boxed) = item else {
                continue;
            };
            let section = boxed.as_ref();
            let Section::Description { subpkg, data, .. } = section else {
                continue;
            };
            if subpkg.is_some() {
                continue; // Subpackages out of scope (see module docs).
            }
            let desc_len = description_text_len(section);
            if desc_len < summary_len {
                self.diagnostics.push(Diagnostic::new(
                    &METADATA,
                    Severity::Allow,
                    format!(
                        "%description is {desc_len} chars but Summary is {summary_len} — \
                         expand the description or fix the placeholder"
                    ),
                    *data,
                ));
            }
        }
        // Mark `visit` as still-used so future maintainers know the
        // walker module is imported deliberately.
        let _ = visit::walk_spec::<Self>;
    }
}

impl Lint for DescriptionShorterThanSummary {
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
        let mut lint = DescriptionShorterThanSummary::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_short_description() {
        let diags = run("Name: x\nSummary: A reasonable two-line summary\n%description\nTODO\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM059");
    }

    #[test]
    fn silent_when_description_longer_than_summary() {
        let src = "Name: x\nSummary: Short\n%description\n\
A multi-line, comfortably longer description here.\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_no_summary() {
        // Without a Summary we can't compare; rule stays quiet.
        let src = "Name: x\n%description\nTODO\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn skips_subpackage_descriptions() {
        // `%description -n foo` is currently out of scope; only main
        // package gets checked.
        let src = "Name: main\nSummary: A multi-word headline string here\n\
%description\nA properly long main description with plenty of words.\n\
%package -n foo\nSummary: x\n%description -n foo\nTODO\n";
        assert!(run(src).is_empty(), "got {:?}", run(src));
    }
}
