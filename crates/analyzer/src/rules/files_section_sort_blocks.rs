//! RPM517 `files-section-sort-blocks` — suggest a canonical ordering
//! of entry blocks inside `%files`: `%license` first, then `%doc`,
//! then `%config`, then everything else.

use rpm_spec::ast::{FileEntry, FilesContent, Section, Span, SpecFile, SpecItem};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM517",
    name: "files-section-sort-blocks",
    description: "`%files` entries are not in the canonical order \
                  (license → doc → config → other) — group them for easier review.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// `%files` entries are not in the canonical order (license → doc → config → other) — group them for easier review.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct FilesSectionSortBlocks {
    diagnostics: Vec<Diagnostic>,
}

impl FilesSectionSortBlocks {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for FilesSectionSortBlocks {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for item in &spec.items {
            let SpecItem::Section(boxed) = item else {
                continue;
            };
            let Section::Files { content, data, .. } = boxed.as_ref() else {
                continue;
            };
            let weights = collect_weights(content);
            if !is_monotonic(&weights) {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "`%files` entries are out of canonical order \
                         (license → doc → config → other) — group them by kind",
                        *data,
                    )
                    .with_suggestion(Suggestion::new(
                        "rearrange the entries so each block appears once and in canonical order",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

fn collect_weights(content: &[FilesContent<Span>]) -> Vec<u8> {
    let mut out = Vec::new();
    for it in content {
        if let FilesContent::Entry(e) = it
            && let Some(w) = weight_of(e)
        {
            out.push(w);
        }
    }
    out
}

fn weight_of(e: &FileEntry<Span>) -> Option<u8> {
    use rpm_spec::ast::FileDirective;
    let mut is_license = false;
    let mut is_doc = false;
    let mut is_config = false;
    for d in &e.directives {
        match d {
            FileDirective::License => is_license = true,
            FileDirective::Doc => is_doc = true,
            FileDirective::Config(_) => is_config = true,
            _ => {}
        }
    }
    if is_license {
        Some(1)
    } else if is_doc {
        Some(2)
    } else if is_config {
        Some(3)
    } else {
        Some(4)
    }
}

fn is_monotonic(weights: &[u8]) -> bool {
    weights.windows(2).all(|w| w[0] <= w[1])
}

impl Lint for FilesSectionSortBlocks {
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
        run_lint::<FilesSectionSortBlocks>(src)
    }

    #[test]
    fn flags_doc_before_license() {
        let src = "Name: x\n%files\n%doc README\n%license LICENSE\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM517");
    }

    #[test]
    fn flags_binary_then_license() {
        let src = "Name: x\n%files\n/usr/bin/foo\n%license LICENSE\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_canonical_order() {
        let src =
            "Name: x\n%files\n%license LICENSE\n%doc README\n%config /etc/foo.conf\n/usr/bin/foo\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_pure_binary_listing() {
        let src = "Name: x\n%files\n/usr/bin/foo\n/usr/bin/bar\n";
        assert!(run(src).is_empty());
    }
}
