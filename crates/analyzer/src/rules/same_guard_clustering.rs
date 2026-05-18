//! RPM454 `same-guard-clustering-in-commutative-context` — flag two or
//! more non-adjacent `%if A` blocks at the same preamble level that
//! share a condition and whose bodies are commutative (dep tags only).
//!
//! Such scattered guards can be clustered into a single `%if A` block
//! containing every guarded entry, removing duplicated `%if`/`%endif`
//! noise and making the dependency story easier to scan.
//!
//! Distinct from RPM076 (`adjacent-mergeable-conditionals`), which
//! handles the *adjacent* case; RPM454 fires only when the blocks are
//! separated by at least one unrelated item.

use rpm_spec::ast::{
    CondExpr, CondKind, Conditional, PreambleContent, Section, Span, SpecFile, SpecItem, Tag,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{DepTagKey, cond_expr_resolvably_eq};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM454",
    name: "same-guard-clustering-in-commutative-context",
    description: "Multiple non-adjacent `%if A` blocks at the same preamble level share a \
                  condition and contain only commutative items — cluster into one `%if A` block.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Multiple non-adjacent `%if A` blocks at the same preamble level share a condition and contain only commutative items — cluster into one `%if A` block.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct SameGuardClustering {
    diagnostics: Vec<Diagnostic>,
}

impl SameGuardClustering {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for SameGuardClustering {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        scan_top_items(&spec.items, &mut self.diagnostics);
        for item in &spec.items {
            let SpecItem::Section(boxed) = item else {
                continue;
            };
            let Section::Package { content, .. } = boxed.as_ref() else {
                continue;
            };
            scan_preamble_content(content, &mut self.diagnostics);
        }
    }
}

#[derive(Debug)]
struct Member<'a> {
    idx: usize,
    span: Span,
    expr: &'a CondExpr<Span>,
}

fn scan_top_items<'a>(items: &'a [SpecItem<Span>], diagnostics: &mut Vec<Diagnostic>) {
    let mut members: Vec<Member<'a>> = Vec::new();
    for (idx, it) in items.iter().enumerate() {
        if let SpecItem::Conditional(c) = it
            && check_top_conditional(c)
        {
            members.push(Member {
                idx,
                span: c.data,
                expr: &c.branches[0].expr,
            });
        }
        // Recurse into branches.
        if let SpecItem::Conditional(c) = it {
            for branch in &c.branches {
                scan_top_items(&branch.body, diagnostics);
            }
            if let Some(els) = &c.otherwise {
                scan_top_items(els, diagnostics);
            }
        }
    }
    emit_groups(
        &members,
        items.len(),
        |i| item_is_filler_top(items.get(i)),
        diagnostics,
    );
}

fn scan_preamble_content<'a>(
    items: &'a [PreambleContent<Span>],
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut members: Vec<Member<'a>> = Vec::new();
    for (idx, it) in items.iter().enumerate() {
        if let PreambleContent::Conditional(c) = it
            && check_preamble_conditional(c)
        {
            members.push(Member {
                idx,
                span: c.data,
                expr: &c.branches[0].expr,
            });
        }
        if let PreambleContent::Conditional(c) = it {
            for branch in &c.branches {
                scan_preamble_content(&branch.body, diagnostics);
            }
            if let Some(els) = &c.otherwise {
                scan_preamble_content(els, diagnostics);
            }
        }
    }
    emit_groups(
        &members,
        items.len(),
        |i| item_is_filler_preamble(items.get(i)),
        diagnostics,
    );
}

fn item_is_filler_top(it: Option<&SpecItem<Span>>) -> bool {
    matches!(it, Some(SpecItem::Blank | SpecItem::Comment(_)) | None)
}

fn item_is_filler_preamble(it: Option<&PreambleContent<Span>>) -> bool {
    matches!(
        it,
        Some(PreambleContent::Blank | PreambleContent::Comment(_)) | None
    )
}

/// Group members by condition equality and emit on groups with 2+
/// non-adjacent entries. "Adjacent" = no non-filler item lies between
/// the two member indices in the parent list.
fn emit_groups<F>(
    members: &[Member<'_>],
    _level_len: usize,
    is_filler_at: F,
    diagnostics: &mut Vec<Diagnostic>,
) where
    F: Fn(usize) -> bool,
{
    let mut used = vec![false; members.len()];
    for i in 0..members.len() {
        if used[i] {
            continue;
        }
        let mut group: Vec<&Member<'_>> = vec![&members[i]];
        used[i] = true;
        for (j, m) in members.iter().enumerate().skip(i + 1) {
            if used[j] {
                continue;
            }
            if cond_expr_resolvably_eq(group[0].expr, m.expr) {
                group.push(m);
                used[j] = true;
            }
        }
        if group.len() < 2 {
            continue;
        }
        // Need at least one non-filler item between two members for
        // "scattered" to apply.
        let mut scattered = false;
        for w in group.windows(2) {
            let a = w[0].idx;
            let b = w[1].idx;
            if (a + 1..b).any(|k| !is_filler_at(k)) {
                scattered = true;
                break;
            }
        }
        if !scattered {
            continue;
        }
        // Emit on every member after the first.
        for m in &group[1..] {
            diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    "scattered `%if` blocks share this condition and contain only commutative \
                     items; cluster them into one `%if` block",
                    m.span,
                )
                .with_suggestion(Suggestion::new(
                    "move every guarded item into a single `%if A … %endif` block",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

fn check_top_conditional(node: &Conditional<Span, SpecItem<Span>>) -> bool {
    if !is_simple_if(node) {
        return false;
    }
    node.branches[0].body.iter().all(|it| match it {
        SpecItem::Preamble(p) => is_commutative_tag(&p.tag),
        SpecItem::Blank | SpecItem::Comment(_) => true,
        _ => false,
    })
}

fn check_preamble_conditional(node: &Conditional<Span, PreambleContent<Span>>) -> bool {
    if !is_simple_if(node) {
        return false;
    }
    node.branches[0].body.iter().all(|it| match it {
        PreambleContent::Item(p) => is_commutative_tag(&p.tag),
        PreambleContent::Blank | PreambleContent::Comment(_) => true,
        _ => false,
    })
}

fn is_simple_if<B>(node: &Conditional<Span, B>) -> bool {
    node.branches.len() == 1
        && matches!(node.branches[0].kind, CondKind::If)
        && node.otherwise.is_none()
}

fn is_commutative_tag(t: &Tag) -> bool {
    // "Commutative" here means a dep-carrying tag whose order on
    // disk doesn't matter — every variant of [`DepTagKey`] qualifies.
    DepTagKey::from_tag(t).is_some()
}

impl Lint for SameGuardClustering {
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
        run_lint::<SameGuardClustering>(src)
    }

    #[test]
    fn flags_scattered_same_condition_blocks() {
        let src = "Name: x\n\
%if A\n\
BuildRequires: foo\n\
%endif\n\
Source0: tarball.tar\n\
%if A\n\
BuildRequires: bar\n\
%endif\n";
        let diags = run(src);
        let hits: Vec<_> = diags.iter().filter(|d| d.lint_id == "RPM454").collect();
        assert_eq!(hits.len(), 1, "{diags:?}");
    }

    #[test]
    fn silent_for_adjacent_same_condition() {
        // RPM076 territory — no intermediate non-filler items.
        let src = "Name: x\n\
%if A\nBuildRequires: foo\n%endif\n\
%if A\nBuildRequires: bar\n%endif\n";
        let diags = run(src);
        assert!(diags.iter().all(|d| d.lint_id != "RPM454"), "{diags:?}");
    }

    #[test]
    fn silent_for_different_conditions() {
        let src = "Name: x\n\
%if A\nBuildRequires: foo\n%endif\n\
Source0: tarball.tar\n\
%if B\nBuildRequires: bar\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_non_commutative_body() {
        // Body contains a non-dep item — order may matter.
        let src = "Name: x\n\
%if A\nSource0: a.tar\n%endif\n\
Version: 1\n\
%if A\nSource1: b.tar\n%endif\n";
        let diags = run(src);
        assert!(diags.iter().all(|d| d.lint_id != "RPM454"));
    }
}
