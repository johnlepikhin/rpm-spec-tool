//! RPM513 `files-directory-subsumes-child` — flag explicit `%files`
//! entries whose path lives under another entry that already owns the
//! parent directory and everything under it.
//!
//! Conservative scope:
//! - The parent entry must NOT carry `%dir` (in which case it owns the
//!   directory but not its contents).
//! - The child entry must NOT carry `%config`, `%doc`, `%license` or
//!   `%ghost` — those mark intentional per-file metadata that should
//!   stay even when the parent already owns the path.
//! - Only paths within one `%files` section are compared; cross-section
//!   collisions are RPM366's territory.

use rpm_spec::ast::{FileEntry, FilesContent, Section, Span, SpecFile, SpecItem};
use rpm_spec_profile::Profile;

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::files::FilesClassifier;
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM513",
    name: "files-directory-subsumes-child",
    description: "A `%files` entry lists a path already covered by another entry that owns the \
                  parent directory and its contents.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// A `%files` entry lists a path already covered by another entry that owns the parent directory and its contents.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct FilesDirectorySubsumesChild {
    diagnostics: Vec<Diagnostic>,
    profile: Profile,
}

impl FilesDirectorySubsumesChild {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for FilesDirectorySubsumesChild {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let classifier = FilesClassifier::new(&self.profile);
        for item in &spec.items {
            let SpecItem::Section(boxed) = item else {
                continue;
            };
            let Section::Files { content, .. } = boxed.as_ref() else {
                continue;
            };
            // Collect (path, span, kind) tuples once per section.
            let entries: Vec<EntryView> = collect_entries(content, &classifier);
            // Hoist `"<parent.path>/"` strings out of the inner loop so
            // each entry's slash-suffixed form is allocated once, not
            // O(N) times during the cross-product scan.
            let parent_with_slash: Vec<String> =
                entries.iter().map(|e| format!("{}/", e.path)).collect();
            for (i, child) in entries.iter().enumerate() {
                if !child.is_eligible_child() {
                    continue;
                }
                for (j, parent) in entries.iter().enumerate() {
                    if i == j {
                        continue;
                    }
                    if !parent.is_eligible_parent() {
                        continue;
                    }
                    if !child.path.starts_with(&parent_with_slash[j]) {
                        continue;
                    }
                    self.diagnostics.push(
                        Diagnostic::new(
                            &METADATA,
                            Severity::Warn,
                            format!(
                                "`{child_path}` is already covered by `{parent_path}` which owns \
                                 the parent directory and its contents",
                                child_path = child.path,
                                parent_path = parent.path,
                            ),
                            child.span,
                        )
                        .with_suggestion(Suggestion::new(
                            "drop the child entry, or mark the parent with `%dir` if you want \
                             to own the directory but not its contents",
                            Vec::new(),
                            Applicability::Manual,
                        )),
                    );
                    break;
                }
            }
        }
    }
}

struct EntryView {
    path: String,
    span: Span,
    is_dir_directive: bool,
    has_special: bool,
}

impl EntryView {
    fn is_eligible_parent(&self) -> bool {
        // Must own dir and contents — not %dir-only.
        !self.is_dir_directive
    }
    fn is_eligible_child(&self) -> bool {
        // Skip children with intentional directives.
        !self.has_special
    }
}

fn collect_entries(
    content: &[FilesContent<Span>],
    classifier: &FilesClassifier<'_>,
) -> Vec<EntryView> {
    let mut out = Vec::new();
    fn rec(
        content: &[FilesContent<Span>],
        classifier: &FilesClassifier<'_>,
        out: &mut Vec<EntryView>,
    ) {
        for it in content {
            match it {
                #[allow(clippy::collapsible_match)]
                FilesContent::Entry(e) => {
                    if let Some(view) = build_view(e, classifier) {
                        out.push(view);
                    }
                }
                FilesContent::Conditional(c) => {
                    for branch in &c.branches {
                        rec(&branch.body, classifier, out);
                    }
                    if let Some(els) = &c.otherwise {
                        rec(els, classifier, out);
                    }
                }
                _ => {}
            }
        }
    }
    rec(content, classifier, &mut out);
    out
}

fn build_view(e: &FileEntry<Span>, classifier: &FilesClassifier<'_>) -> Option<EntryView> {
    let cls = classifier.classify(e);
    let path = cls.resolved_path?;
    if path.contains('*') || path.contains('?') || path.contains('[') {
        return None;
    }
    let dirs = &cls.directives;
    let has_special = dirs.is_doc || dirs.is_license || dirs.is_ghost || dirs.config.is_some();
    Some(EntryView {
        path,
        span: e.data,
        is_dir_directive: dirs.is_dir,
        has_special,
    })
}

impl Lint for FilesDirectorySubsumesChild {
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
        run_lint::<FilesDirectorySubsumesChild>(src)
    }

    #[test]
    fn flags_child_under_parent_dir() {
        let src = "Name: x\n%files\n/usr/share/foo\n/usr/share/foo/bar\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM513");
    }

    #[test]
    fn silent_when_parent_marked_dir() {
        // `%dir parent` only owns the directory; child is intentional.
        let src = "Name: x\n%files\n%dir /usr/share/foo\n/usr/share/foo/bar\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_child_is_config() {
        let src = "Name: x\n%files\n/usr/share/foo\n%config /usr/share/foo/bar\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_no_parent_relation() {
        let src = "Name: x\n%files\n/usr/share/foo\n/usr/share/baz\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_two_unrelated_files_no_quadratic_alloc() {
        // Regression cover for the hoisted `parent_with_slash` cache:
        // two unrelated entries must not trigger and the cache must
        // produce identical results to the per-iteration `format!`.
        let src = "Name: x\n%files\n/usr/bin/foo\n/usr/bin/bar\n/etc/foo.conf\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_under_deeper_parent_still_works_after_hoist() {
        // Multiple entries — parent at index 0, child at index 2.
        // Confirms the `parent_with_slash[j]` indexing matches the
        // right parent.
        let src = "Name: x\n%files\n/opt/app\n/etc/app.conf\n/opt/app/bin/run\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("/opt/app/bin/run"));
        assert!(diags[0].message.contains("/opt/app"));
    }
}
