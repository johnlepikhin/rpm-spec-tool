//! RPM516 `repeated-files-prefix-to-directory-entry` — flag `%files`
//! sections where many entries live under the same private directory;
//! a single `dirpath` entry that owns the whole tree may be shorter
//! and easier to maintain.
//!
//! Skips entries with directives (`%doc`, `%license`, `%config`,
//! `%ghost`) — those are intentional per-file metadata.

use std::collections::HashMap;

use rpm_spec::ast::{FileEntry, FilesContent, Section, Span, SpecFile, SpecItem};
use rpm_spec_profile::Profile;

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::files::FilesClassifier;
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM516",
    name: "repeated-files-prefix-to-directory-entry",
    description: "Many `%files` entries share a deep private directory — consider one entry that \
                  owns the whole directory instead.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// Need at least this many entries under one parent to suggest the
/// directory rewrite. Smaller groups don't pay back.
const THRESHOLD: usize = 4;
/// Parent path must contain at least this many `/` separators to be
/// considered a "private" subdirectory worth owning outright. This
/// keeps the rule away from top-level dirs like `/usr/bin`.
const MIN_DEPTH: usize = 3;

/// Many `%files` entries share a deep private directory — consider one entry that owns the whole directory instead.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct RepeatedFilesPrefix {
    diagnostics: Vec<Diagnostic>,
    profile: Profile,
}

impl RepeatedFilesPrefix {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for RepeatedFilesPrefix {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let classifier = FilesClassifier::new(&self.profile);
        for item in &spec.items {
            let SpecItem::Section(boxed) = item else {
                continue;
            };
            let Section::Files { content, data, .. } = boxed.as_ref() else {
                continue;
            };
            let mut by_parent: HashMap<String, usize> = HashMap::new();
            collect_parents(content, &classifier, &mut by_parent);
            for (parent, count) in by_parent {
                if count < THRESHOLD {
                    continue;
                }
                if parent_depth(&parent) < MIN_DEPTH {
                    continue;
                }
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        format!(
                            "{count} entries under `{parent}` — consider listing the directory \
                             itself and letting it own the tree"
                        ),
                        *data,
                    )
                    .with_suggestion(Suggestion::new(
                        format!("replace the per-file entries with a single `{parent}` entry"),
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

fn collect_parents(
    content: &[FilesContent<Span>],
    classifier: &FilesClassifier<'_>,
    out: &mut HashMap<String, usize>,
) {
    for it in content {
        match it {
            #[allow(clippy::collapsible_match)]
            FilesContent::Entry(e) => {
                if let Some(parent) = parent_of(e, classifier) {
                    *out.entry(parent).or_insert(0) += 1;
                }
            }
            FilesContent::Conditional(c) => {
                for branch in &c.branches {
                    collect_parents(&branch.body, classifier, out);
                }
                if let Some(els) = &c.otherwise {
                    collect_parents(els, classifier, out);
                }
            }
            _ => {}
        }
    }
}

fn parent_of(e: &FileEntry<Span>, classifier: &FilesClassifier<'_>) -> Option<String> {
    let cls = classifier.classify(e);
    let path = cls.resolved_path?;
    if path.contains('*') || path.contains('?') || path.contains('[') {
        return None;
    }
    let d = &cls.directives;
    if d.is_doc || d.is_license || d.is_ghost || d.is_dir || d.config.is_some() {
        return None;
    }
    let parent_end = path.rfind('/')?;
    if parent_end == 0 {
        return None;
    }
    Some(path[..parent_end].to_owned())
}

fn parent_depth(parent: &str) -> usize {
    parent.matches('/').count()
}

impl Lint for RepeatedFilesPrefix {
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
    use crate::rules::test_support::run_lint;

    fn run(src: &str) -> Vec<Diagnostic> {
        run_lint::<RepeatedFilesPrefix>(src)
    }

    #[test]
    fn flags_four_entries_under_same_deep_dir() {
        let src = "Name: x\n%files\n\
/usr/share/foo/a\n/usr/share/foo/b\n/usr/share/foo/c\n/usr/share/foo/d\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM516");
    }

    #[test]
    fn silent_below_threshold() {
        let src = "Name: x\n%files\n/usr/share/foo/a\n/usr/share/foo/b\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_shallow_dir() {
        // /usr/bin has depth 2 — below MIN_DEPTH; don't suggest owning it.
        let src = "Name: x\n%files\n/usr/bin/a\n/usr/bin/b\n/usr/bin/c\n/usr/bin/d\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_entries_have_directives() {
        let src = "Name: x\n%files\n\
%config /usr/share/foo/a\n%config /usr/share/foo/b\n%config /usr/share/foo/c\n%config /usr/share/foo/d\n";
        assert!(run(src).is_empty());
    }
}
