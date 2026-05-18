//! RPM555 `description-equals-summary` — flag a `%description` body
//! that is byte-for-byte the same as the `Summary:` tag value.
//!
//! Duplicating the summary inside `%description` wastes the
//! description slot — write longer prose explaining the package.

use rpm_spec::ast::{Section, Span, SpecFile, SpecItem, Tag, TagValue, TextBody};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::collect_top_level_preamble;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM555",
    name: "description-equals-summary",
    description: "`%description` body is byte-for-byte identical to the `Summary:` tag — write \
                  prose that adds context beyond the summary.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// `%description` body is byte-for-byte identical to the `Summary:` tag — write prose that adds context beyond the summary.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct DescriptionEqualsSummary {
    diagnostics: Vec<Diagnostic>,
}

impl DescriptionEqualsSummary {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for DescriptionEqualsSummary {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let Some(summary) = main_summary(spec) else {
            return;
        };
        let summary = summary.trim();
        if summary.is_empty() {
            return;
        }
        for item in &spec.items {
            let SpecItem::Section(boxed) = item else {
                continue;
            };
            let Section::Description { subpkg, body, data } = boxed.as_ref() else {
                continue;
            };
            if subpkg.is_some() {
                continue;
            }
            let canon = canonical_body(body);
            if canon.trim() == summary {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "`%description` body is identical to `Summary:` — write a description \
                         that goes beyond the summary",
                        *data,
                    )
                    .with_suggestion(Suggestion::new(
                        "expand the `%description` with context the summary can't fit",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

fn main_summary(spec: &SpecFile<Span>) -> Option<String> {
    for item in collect_top_level_preamble(spec) {
        if matches!(item.tag, Tag::Summary)
            && let TagValue::Text(t) = &item.value
            && let Some(s) = t.literal_str()
        {
            return Some(s.to_owned());
        }
    }
    None
}

fn canonical_body(body: &TextBody) -> String {
    let mut out = String::new();
    for (i, line) in body.lines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if let Some(s) = line.literal_str() {
            out.push_str(s.trim_end());
        }
    }
    out.trim().to_owned()
}

impl Lint for DescriptionEqualsSummary {
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
        run_lint::<DescriptionEqualsSummary>(src)
    }

    #[test]
    fn flags_identical_description_and_summary() {
        let src = "Name: x\nSummary: Acme tools\n%description\nAcme tools\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM555");
    }

    #[test]
    fn silent_for_distinct() {
        let src = "Name: x\nSummary: Acme tools\n%description\nAcme tools for X.\n";
        assert!(run(src).is_empty());
    }
}
