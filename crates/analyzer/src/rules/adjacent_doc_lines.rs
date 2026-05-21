//! RPM510 `adjacent-doc-lines-merge` — flag adjacent `%doc PATH`
//! entries within one `%files` section that can be folded into a
//! single `%doc A B C` line.
//!
//! Safe within one section because `%doc` accepts an arbitrary number
//! of paths on one line; merging shortens the spec without changing
//! behaviour.

use rpm_spec::ast::{FileEntry, FilesContent, Section, Span, SpecFile, SpecItem};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM510",
    name: "adjacent-doc-lines-merge",
    description: "Adjacent `%doc PATH` entries within one `%files` section — merge into a single \
                  `%doc A B C` line.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Adjacent `%doc PATH` entries within one `%files` section — merge into a single `%doc A B C` line.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct AdjacentDocLines {
    diagnostics: Vec<Diagnostic>,
}

impl AdjacentDocLines {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for AdjacentDocLines {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for item in &spec.items {
            let SpecItem::Section(boxed) = item else {
                continue;
            };
            let Section::Files { content, .. } = boxed.as_ref() else {
                continue;
            };
            self.scan_files_content(content);
        }
    }
}

impl AdjacentDocLines {
    fn scan_files_content(&mut self, content: &[FilesContent<Span>]) {
        let mut prev_doc: Option<Span> = None;
        for it in content {
            match it {
                FilesContent::Entry(e) if is_doc_entry_with_path(e) => {
                    if prev_doc.is_some() {
                        self.diagnostics.push(
                            Diagnostic::new(
                                &METADATA,
                                Severity::Warn,
                                "adjacent `%doc` entries — merge into one `%doc A B C` line",
                                e.data,
                            )
                            .with_suggestion(Suggestion::new(
                                "combine consecutive `%doc PATH` entries into a single \
                                     `%doc PATH1 PATH2 …` line",
                                Vec::new(),
                                Applicability::Manual,
                            )),
                        );
                    }
                    prev_doc = Some(e.data);
                }
                FilesContent::Entry(_) => {
                    prev_doc = None;
                }
                FilesContent::Blank | FilesContent::Comment(_) => {
                    // Whitespace doesn't break adjacency.
                }
                FilesContent::Conditional(c) => {
                    prev_doc = None;
                    for branch in &c.branches {
                        self.scan_files_content(&branch.body);
                    }
                    if let Some(els) = &c.otherwise {
                        self.scan_files_content(els);
                    }
                }
                _ => {
                    prev_doc = None;
                }
            }
        }
    }
}

fn is_doc_entry_with_path(e: &FileEntry<Span>) -> bool {
    use rpm_spec::ast::FileDirective;
    if e.path.is_none() {
        return false;
    }
    // Must have %doc and no incompatible/qualifier directive that would
    // make merging unsafe. `%lang(...)`, `%attr(...)`, `%caps(...)`,
    // `%verify(...)`, `%config(...)`, `%defattr(...)`, `%artifact`,
    // `%missingok` all carry per-line metadata that does not survive a
    // naive merge into a single `%doc A B C` line — merging would
    // silently drop the qualifier or apply it to the wrong paths.
    // Real-world repro: `%doc README` adjacent to `%lang(ja) %doc README.ja`
    // (e.g. ruby.spec) must not be merged.
    let mut has_doc = false;
    for d in &e.directives {
        match d {
            FileDirective::Doc => has_doc = true,
            FileDirective::License
            | FileDirective::Dir
            | FileDirective::Ghost
            | FileDirective::Lang(_)
            | FileDirective::Caps(_)
            | FileDirective::Attr(_)
            | FileDirective::Defattr(_)
            | FileDirective::Config(_)
            | FileDirective::Verify { .. }
            | FileDirective::Artifact
            | FileDirective::MissingOk => return false,
            // `FileDirective` is `#[non_exhaustive]`; default to
            // refusing the merge for any future variant — safer than
            // silently merging through an unknown qualifier.
            _ => return false,
        }
    }
    has_doc
}

impl Lint for AdjacentDocLines {
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
        run_lint::<AdjacentDocLines>(src)
    }

    #[test]
    fn flags_adjacent_doc_pair() {
        let src = "Name: x\n%files\n%doc README\n%doc NEWS\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM510");
    }

    #[test]
    fn flags_three_adjacent_doc_lines() {
        let src = "Name: x\n%files\n%doc README\n%doc NEWS\n%doc CHANGES\n";
        let diags = run(src);
        // Two adjacency events (NEWS after README, CHANGES after NEWS).
        assert_eq!(diags.len(), 2, "{diags:?}");
    }

    #[test]
    fn silent_for_single_doc() {
        let src = "Name: x\n%files\n%doc README\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_separated_by_non_doc_entry() {
        let src = "Name: x\n%files\n%doc README\n%{_bindir}/foo\n%doc NEWS\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_doc_and_license_mix() {
        // `%license` is a separate directive — not RPM510's case.
        let src = "Name: x\n%files\n%doc README\n%license LICENSE\n";
        assert!(run(src).is_empty());
    }

    /// `%lang(ja) %doc README.ja` carries a locale qualifier that does
    /// not survive a naive merge into a single `%doc A B C` line. The
    /// rule must skip any entry whose directives include `%lang(...)`
    /// (or any other per-line qualifier). Adjacency must not span
    /// across such a line either — at most a single diagnostic is
    /// acceptable, jumping over the `%lang` entry.
    #[test]
    fn silent_when_lang_qualifier_between_doc_lines() {
        let src = "Name: x\n\
%files\n\
%doc README\n\
%lang(ja) %doc README.ja\n\
%doc CHANGELOG\n";
        let diags = run(src);
        assert!(
            diags.len() <= 1,
            "must not merge across %lang qualifier; got {diags:?}"
        );
    }
}
