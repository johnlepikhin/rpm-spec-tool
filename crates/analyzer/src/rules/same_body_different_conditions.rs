//! RPM445 `same-body-different-conditions-merge` — flag two adjacent
//! top-level `%if` blocks whose conditions differ but whose bodies are
//! source-identical.
//!
//! ```text
//! %if A
//! BuildRequires: foo
//! %endif
//! %if B
//! BuildRequires: foo
//! %endif
//! ```
//! Merging into `%if A || B` removes a duplicate copy of the body and
//! makes the OR explicit.
//!
//! Distinct from:
//! - RPM076 (`adjacent-mergeable-conditionals`): adjacent blocks with
//!   the *same* condition.
//! - RPM099 (`merge-elif-same-body`): two `%elif` arms of one chain
//!   with the same body.
//!
//! RPM445 spans two *separate* top-level `%if` blocks.

use rpm_spec::ast::{
    CondKind, Conditional, FilesContent, PreambleContent, Span, SpecFile, SpecItem,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::conditional_merge::{HasBodySpan, bodies_source_eq};
use crate::rules::util::cond_expr_resolvably_eq;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM445",
    name: "same-body-different-conditions-merge",
    description: "Two adjacent `%if` blocks have the same body but different conditions — merge \
                  them into one block with the conditions joined by `||`.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Two adjacent `%if` blocks have the same body but different conditions — merge them into one block with the conditions joined by `||`.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct SameBodyDifferentConditions {
    diagnostics: Vec<Diagnostic>,
    source: std::sync::Arc<str>,
}

impl SameBodyDifferentConditions {
    pub fn new() -> Self {
        Self::default()
    }

    fn check_pair<B: HasBodySpan>(
        &mut self,
        prev: &Conditional<Span, B>,
        curr: &Conditional<Span, B>,
    ) {
        if !is_simple_if(prev) || !is_simple_if(curr) {
            return;
        }
        let prev_branch = &prev.branches[0];
        let curr_branch = &curr.branches[0];
        // Same condition → RPM076's territory; don't double-warn.
        if cond_expr_resolvably_eq(&prev_branch.expr, &curr_branch.expr) {
            return;
        }
        // Both branches are `%ifarch` → RPM446's territory; don't
        // double-warn (and `||` is the wrong rewrite for arch lists).
        if matches!(prev_branch.kind, CondKind::IfArch)
            && matches!(curr_branch.kind, CondKind::IfArch)
        {
            return;
        }
        if !bodies_source_eq(&prev_branch.body, &curr_branch.body, &self.source) {
            return;
        }
        self.diagnostics.push(
            Diagnostic::new(
                &METADATA,
                Severity::Warn,
                "adjacent `%if` blocks share the same body but have different conditions; \
                 merge them and join the conditions with `||`",
                curr.data,
            )
            .with_suggestion(Suggestion::new(
                "fold both blocks into one `%if A || B … %endif`",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

/// `true` for a bare `%if EXPR …%endif` — one branch, no `%elif`, no
/// `%else`. Only simple shapes merge cleanly.
fn is_simple_if<B>(node: &Conditional<Span, B>) -> bool {
    node.branches.len() == 1
        && matches!(node.branches[0].kind, CondKind::If)
        && node.otherwise.is_none()
}

impl<'ast> Visit<'ast> for SameBodyDifferentConditions {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        scan_top_items::<SpecItem<Span>>(&spec.items, self, |it| {
            if let SpecItem::Conditional(c) = it {
                Some(c)
            } else {
                None
            }
        });
        scan_top_items_recurse(&spec.items, self);
    }
}

/// Scan a slice of `B` items looking for adjacent conditional pairs.
/// `extract` projects an item into a borrowed Conditional or `None`.
fn scan_top_items<B>(
    items: &[B],
    rule: &mut SameBodyDifferentConditions,
    extract: impl Fn(&B) -> Option<&Conditional<Span, B>>,
) where
    B: HasBodySpan,
{
    let mut prev: Option<&Conditional<Span, B>> = None;
    for item in items {
        match extract(item) {
            Some(c) => {
                if let Some(p) = prev {
                    rule.check_pair(p, c);
                }
                prev = Some(c);
            }
            None => {
                // Skip Blanks/Comments — they don't break adjacency.
                if !is_filler(item) {
                    prev = None;
                }
            }
        }
    }
}

fn is_filler<B>(item: &B) -> bool
where
    B: HasBodySpan,
{
    // Items without a body_span are blanks; treat as transparent for
    // adjacency. This matches how RPM076 / RPM099 reason.
    item.body_span().is_none()
}

/// Recurse into nested conditional branches so the rule also fires on
/// adjacent inner `%if`s inside a larger conditional.
fn scan_top_items_recurse(items: &[SpecItem<Span>], rule: &mut SameBodyDifferentConditions) {
    for it in items {
        match it {
            SpecItem::Conditional(c) => {
                for branch in &c.branches {
                    scan_top_items::<SpecItem<Span>>(&branch.body, rule, |x| {
                        if let SpecItem::Conditional(c) = x {
                            Some(c)
                        } else {
                            None
                        }
                    });
                    scan_top_items_recurse(&branch.body, rule);
                }
                if let Some(els) = &c.otherwise {
                    scan_top_items::<SpecItem<Span>>(els, rule, |x| {
                        if let SpecItem::Conditional(c) = x {
                            Some(c)
                        } else {
                            None
                        }
                    });
                    scan_top_items_recurse(els, rule);
                }
            }
            SpecItem::Section(boxed) => match boxed.as_ref() {
                rpm_spec::ast::Section::Package { content, .. } => {
                    scan_preamble_content_pairs(content, rule);
                }
                rpm_spec::ast::Section::Files { content, .. } => {
                    scan_files_content_pairs(content, rule);
                }
                _ => {}
            },
            _ => {}
        }
    }
}

fn scan_preamble_content_pairs(
    items: &[PreambleContent<Span>],
    rule: &mut SameBodyDifferentConditions,
) {
    scan_top_items::<PreambleContent<Span>>(items, rule, |x| {
        if let PreambleContent::Conditional(c) = x {
            Some(c)
        } else {
            None
        }
    });
    for it in items {
        if let PreambleContent::Conditional(c) = it {
            for branch in &c.branches {
                scan_preamble_content_pairs(&branch.body, rule);
            }
            if let Some(els) = &c.otherwise {
                scan_preamble_content_pairs(els, rule);
            }
        }
    }
}

fn scan_files_content_pairs(items: &[FilesContent<Span>], rule: &mut SameBodyDifferentConditions) {
    scan_top_items::<FilesContent<Span>>(items, rule, |x| {
        if let FilesContent::Conditional(c) = x {
            Some(c)
        } else {
            None
        }
    });
    for it in items {
        if let FilesContent::Conditional(c) = it {
            for branch in &c.branches {
                scan_files_content_pairs(&branch.body, rule);
            }
            if let Some(els) = &c.otherwise {
                scan_files_content_pairs(els, rule);
            }
        }
    }
}

impl Lint for SameBodyDifferentConditions {
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
        run_lint::<SameBodyDifferentConditions>(src)
    }

    #[test]
    fn flags_adjacent_blocks_with_same_body() {
        let src = "Name: x\n\
%if A\n\
BuildRequires: foo\n\
%endif\n\
%if B\n\
BuildRequires: foo\n\
%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM445");
    }

    #[test]
    fn silent_for_same_condition() {
        // RPM076 covers this.
        let src = "Name: x\n\
%if A\nBuildRequires: foo\n%endif\n\
%if A\nBuildRequires: foo\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_different_bodies() {
        let src = "Name: x\n\
%if A\nBuildRequires: foo\n%endif\n\
%if B\nBuildRequires: bar\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_blocks_separated_by_other_item() {
        // Non-conditional, non-blank item between → no adjacency.
        let src = "Name: x\n\
%if A\nBuildRequires: foo\n%endif\n\
License: MIT\n\
%if B\nBuildRequires: foo\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn tolerates_blank_lines_between() {
        let src = "Name: x\n\
%if A\nBuildRequires: foo\n%endif\n\
\n\
%if B\nBuildRequires: foo\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn silent_when_first_block_has_else() {
        let src = "Name: x\n\
%if A\nBuildRequires: foo\n%else\nBuildRequires: bar\n%endif\n\
%if B\nBuildRequires: foo\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn fires_inside_subpackage() {
        let src = "Name: x\n\
%package devel\n\
Summary: dev\n\
%if A\nRequires: foo\n%endif\n\
%if B\nRequires: foo\n%endif\n\
%description devel\nbody\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }
}
