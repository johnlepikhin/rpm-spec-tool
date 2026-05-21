//! RPM511 `adjacent-license-lines-merge` — flag adjacent `%license PATH`
//! entries within one `%files` section that can be folded into a
//! single `%license A B C` line.

use rpm_spec::ast::{FileEntry, FilesContent, Section, Span, SpecFile, SpecItem};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM511",
    name: "adjacent-license-lines-merge",
    description: "Adjacent `%license PATH` entries within one `%files` section — merge into a \
                  single `%license A B C` line.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Adjacent `%license PATH` entries within one `%files` section — merge into a single `%license A B C` line.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct AdjacentLicenseLines {
    diagnostics: Vec<Diagnostic>,
}

impl AdjacentLicenseLines {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for AdjacentLicenseLines {
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

impl AdjacentLicenseLines {
    fn scan_files_content(&mut self, content: &[FilesContent<Span>]) {
        let mut prev: Option<Span> = None;
        for it in content {
            match it {
                FilesContent::Entry(e) if is_license_entry_with_path(e) => {
                    if prev.is_some() {
                        self.diagnostics.push(
                            Diagnostic::new(
                                &METADATA,
                                Severity::Warn,
                                "adjacent `%license` entries — merge into one `%license A B C` \
                                     line",
                                e.data,
                            )
                            .with_suggestion(Suggestion::new(
                                "combine consecutive `%license PATH` entries into a single \
                                     `%license PATH1 PATH2 …` line",
                                Vec::new(),
                                Applicability::Manual,
                            )),
                        );
                    }
                    prev = Some(e.data);
                }
                FilesContent::Entry(_) => {
                    prev = None;
                }
                FilesContent::Blank | FilesContent::Comment(_) => {}
                FilesContent::Conditional(c) => {
                    prev = None;
                    for branch in &c.branches {
                        self.scan_files_content(&branch.body);
                    }
                    if let Some(els) = &c.otherwise {
                        self.scan_files_content(els);
                    }
                }
                _ => {
                    prev = None;
                }
            }
        }
    }
}

fn is_license_entry_with_path(e: &FileEntry<Span>) -> bool {
    use rpm_spec::ast::FileDirective;
    if e.path.is_none() {
        return false;
    }
    // Must have %license and no incompatible/qualifier directive that
    // would make merging unsafe. `%lang(...)`, `%attr(...)`, `%caps(...)`,
    // `%verify(...)`, `%config(...)`, `%defattr(...)`, `%artifact`,
    // `%missingok` all carry per-line metadata that does not survive a
    // naive merge into a single `%license A B C` line — merging would
    // silently drop the qualifier or apply it to the wrong paths.
    // Real-world repro: `%license COPYING` adjacent to
    // `%lang(ja) %license COPYING.ja` (e.g. ruby.spec) must not be merged.
    let mut has_license = false;
    for d in &e.directives {
        match d {
            FileDirective::License => has_license = true,
            FileDirective::Doc
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
    has_license
}

impl Lint for AdjacentLicenseLines {
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
        run_lint::<AdjacentLicenseLines>(src)
    }

    #[test]
    fn flags_adjacent_license_pair() {
        let src = "Name: x\n%files\n%license LICENSE\n%license NOTICE\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM511");
    }

    #[test]
    fn silent_for_single_license() {
        let src = "Name: x\n%files\n%license LICENSE\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_separated_pair() {
        let src = "Name: x\n%files\n%license LICENSE\n%{_bindir}/foo\n%license NOTICE\n";
        assert!(run(src).is_empty());
    }

    /// `%lang(ja) %license COPYING.ja` carries a locale qualifier that
    /// does not survive a naive merge into a single `%license A B C`
    /// line. The rule must skip any entry whose directives include
    /// `%lang(...)` (or any other per-line qualifier). Adjacency must
    /// not span across such a line either — at most a single diagnostic
    /// is acceptable, jumping over the `%lang` entry.
    ///
    /// Real repro: `ruby.spec:1236-1239`:
    ///   %files
    ///   %license BSDL
    ///   %license COPYING
    ///   %lang(ja) %license COPYING.ja
    ///   %license GPL
    #[test]
    fn silent_when_lang_qualifier_between_license_lines() {
        let src = "Name: x\n\
%files\n\
%license BSDL\n\
%license COPYING\n\
%lang(ja) %license COPYING.ja\n\
%license GPL\n";
        let diags = run(src);
        assert!(
            diags.len() <= 1,
            "must not merge across %lang qualifier; got {diags:?}"
        );
    }
}
