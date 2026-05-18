//! RPM575 `repeated-comment-before-identical-guards` — flag the same
//! explanatory `# …` comment line repeating before two or more
//! `%if` blocks.
//!
//! When the comment never changes, hoist it to a single position
//! (above the first guard or into the spec header) so future edits
//! don't have to update each copy.

use std::collections::HashMap;

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM575",
    name: "repeated-comment-before-identical-guards",
    description: "The same explanatory comment precedes multiple `%if` blocks — hoist it to a \
                  single location instead of repeating it.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// The same explanatory comment precedes multiple `%if` blocks — hoist it to a single location instead of repeating it.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct RepeatedCommentBeforeGuards {
    diagnostics: Vec<Diagnostic>,
    source: std::sync::Arc<str>,
}

impl RepeatedCommentBeforeGuards {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for RepeatedCommentBeforeGuards {
    fn visit_spec(&mut self, _spec: &'ast SpecFile<Span>) {
        if self.source.is_empty() {
            return;
        }
        // Scan lines once; collect `(comment, before-byte-offset-of-%if)`
        // pairs.
        let mut pairs: Vec<(String, usize)> = Vec::new();
        let mut prev_comment: Option<String> = None;
        let mut byte_cursor = 0usize;
        for line in self.source.lines() {
            let line_start = byte_cursor;
            byte_cursor += line.len() + 1;
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix('#') {
                let body = rest.trim();
                if !body.is_empty() {
                    prev_comment = Some(body.to_owned());
                    continue;
                }
            }
            // Blank line resets nothing — comments above blank then %if
            // don't pair (too loose). Reset on blank.
            if trimmed.is_empty() {
                prev_comment = None;
                continue;
            }
            if trimmed.starts_with("%if")
                && let Some(c) = prev_comment.take()
            {
                pairs.push((c, line_start));
            } else {
                prev_comment = None;
            }
        }
        let mut buckets: HashMap<&str, Vec<usize>> = HashMap::new();
        for (c, off) in &pairs {
            buckets.entry(c.as_str()).or_default().push(*off);
        }
        for (_comment, offsets) in buckets {
            if offsets.len() < 2 {
                continue;
            }
            // Skip the FIRST occurrence; emit on subsequent ones.
            for off in offsets.iter().skip(1) {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "the same comment precedes another `%if` block — hoist it to one place",
                        Span::from_bytes(*off, *off + 1),
                    )
                    .with_suggestion(Suggestion::new(
                        "move the explanatory comment to a single location and drop the copies",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
    fn visit_section(&mut self, _node: &'ast rpm_spec::ast::Section<Span>) {
        // Override default to no-op; we run the entire pass from
        // visit_spec on the raw source.
    }
}

impl Lint for RepeatedCommentBeforeGuards {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_source(&mut self, source: std::sync::Arc<str>) {
        self.source = source;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::test_support::run_lint;

    fn run(src: &str) -> Vec<Diagnostic> {
        run_lint::<RepeatedCommentBeforeGuards>(src)
    }

    #[test]
    fn flags_repeated_comment() {
        let src = "Name: x\n\
# Fedora-specific\n\
%if 0%{?fedora}\nBuildRequires: a\n%endif\n\
# Fedora-specific\n\
%if 0%{?fedora}\nBuildRequires: b\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM575");
    }

    #[test]
    fn silent_for_distinct_comments() {
        let src = "Name: x\n\
# Fedora-specific\n\
%if 0%{?fedora}\nBuildRequires: a\n%endif\n\
# RHEL-specific\n\
%if 0%{?rhel}\nBuildRequires: b\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_comment_unrelated_to_if() {
        let src = "Name: x\n# Some note\nVersion: 1\n# Some note\nRelease: 1\n";
        assert!(run(src).is_empty());
    }
}
