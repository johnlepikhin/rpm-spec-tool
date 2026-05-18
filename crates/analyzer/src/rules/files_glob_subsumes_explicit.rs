//! RPM514 `files-glob-subsumes-explicit-entry` — flag explicit `%files`
//! entries whose path is already matched by a glob entry in the same
//! section.

use rpm_spec::ast::{FileEntry, FilesContent, Section, Span, SpecFile, SpecItem};
use rpm_spec_profile::Profile;

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::files::FilesClassifier;
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM514",
    name: "files-glob-subsumes-explicit-entry",
    description: "An explicit `%files` entry is already matched by a glob entry in the same \
                  section — drop the explicit duplicate.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// An explicit `%files` entry is already matched by a glob entry in the same section — drop the explicit duplicate.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct FilesGlobSubsumesExplicit {
    diagnostics: Vec<Diagnostic>,
    profile: Profile,
}

impl FilesGlobSubsumesExplicit {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for FilesGlobSubsumesExplicit {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let classifier = FilesClassifier::new(&self.profile);
        for item in &spec.items {
            let SpecItem::Section(boxed) = item else {
                continue;
            };
            let Section::Files { content, .. } = boxed.as_ref() else {
                continue;
            };
            let mut globs: Vec<String> = Vec::new();
            let mut explicits: Vec<(String, Span)> = Vec::new();
            collect(content, &classifier, &mut globs, &mut explicits);
            for (path, span) in explicits {
                for g in &globs {
                    if simple_glob_match(g, &path) {
                        self.diagnostics.push(
                            Diagnostic::new(
                                &METADATA,
                                Severity::Warn,
                                format!(
                                    "explicit entry `{path}` is already matched by glob `{g}` in \
                                     the same `%files` section — drop the duplicate"
                                ),
                                span,
                            )
                            .with_suggestion(Suggestion::new(
                                "delete the explicit entry; the glob already covers it",
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
}

fn collect(
    content: &[FilesContent<Span>],
    classifier: &FilesClassifier<'_>,
    globs: &mut Vec<String>,
    explicits: &mut Vec<(String, Span)>,
) {
    for it in content {
        match it {
            FilesContent::Entry(e) => {
                if let Some(view) = build_view(e, classifier) {
                    if view.is_glob {
                        globs.push(view.path);
                    } else if !view.has_special_directive {
                        explicits.push((view.path, e.data));
                    }
                }
            }
            FilesContent::Conditional(c) => {
                for branch in &c.branches {
                    collect(&branch.body, classifier, globs, explicits);
                }
                if let Some(els) = &c.otherwise {
                    collect(els, classifier, globs, explicits);
                }
            }
            _ => {}
        }
    }
}

struct EntryView {
    path: String,
    is_glob: bool,
    has_special_directive: bool,
}

fn build_view(e: &FileEntry<Span>, classifier: &FilesClassifier<'_>) -> Option<EntryView> {
    let cls = classifier.classify(e);
    let is_glob = cls.is_glob();
    let path = cls.resolved_path?;
    let d = &cls.directives;
    let has_special_directive =
        d.is_doc || d.is_license || d.is_ghost || d.is_dir || d.config.is_some();
    Some(EntryView {
        path,
        is_glob,
        has_special_directive,
    })
}

/// Minimal glob matcher supporting `*` (any chars, including `/`) and
/// `?` (one char). Brackets are NOT supported — present-but-unmatched
/// brackets in the pattern reduce to literal comparison.
fn simple_glob_match(pattern: &str, candidate: &str) -> bool {
    let pat = pattern.as_bytes();
    let cand = candidate.as_bytes();
    let (mut pi, mut ci) = (0usize, 0usize);
    let (mut star_pi, mut star_ci) = (usize::MAX, 0usize);
    while ci < cand.len() {
        if pi < pat.len() && (pat[pi] == b'?' || pat[pi] == cand[ci]) {
            pi += 1;
            ci += 1;
        } else if pi < pat.len() && pat[pi] == b'*' {
            star_pi = pi;
            star_ci = ci;
            pi += 1;
        } else if star_pi != usize::MAX {
            pi = star_pi + 1;
            star_ci += 1;
            ci = star_ci;
        } else {
            return false;
        }
    }
    while pi < pat.len() && pat[pi] == b'*' {
        pi += 1;
    }
    pi == pat.len()
}

impl Lint for FilesGlobSubsumesExplicit {
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
        run_lint::<FilesGlobSubsumesExplicit>(src)
    }

    #[test]
    fn flags_explicit_covered_by_glob() {
        let src = "Name: x\n%files\n/usr/bin/foo*\n/usr/bin/foobar\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM514");
    }

    #[test]
    fn silent_when_no_glob_match() {
        let src = "Name: x\n%files\n/usr/bin/foo*\n/usr/lib/bar\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_explicit_has_directive() {
        let src = "Name: x\n%files\n/usr/bin/foo*\n%config /usr/bin/foobar\n";
        assert!(run(src).is_empty());
    }
}
