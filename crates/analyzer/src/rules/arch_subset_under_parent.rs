//! RPM442 `arch-subset-under-parent` — flag inner `%ifarch` blocks
//! whose arch list is a superset of an enclosing `%ifarch` list.
//!
//! ```text
//! %ifarch x86_64 aarch64
//!   …
//!   %ifarch x86_64 aarch64 ppc64le
//!     …
//!   %endif
//! %endif
//! ```
//! The inner test always passes: control already implies the build
//! arch is `x86_64` or `aarch64`, both of which are in the inner list.
//!
//! Distinct from RPM114 (`always-true-branch-under-parent`), which
//! reasons over the boolean atom encoding of arches. That encoding
//! treats each arch literal as an independent boolean and so cannot
//! prove subset-under-superset relationships across `%ifarch` blocks.

use std::collections::BTreeSet;

use rpm_spec::ast::{
    CondExpr, CondKind, Conditional, FilesContent, PreambleContent, Span, SpecFile, SpecItem,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::path_cond::{walk_files_body, walk_preamble_body, walk_top_body};
use crate::rules::util::literal_archs;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM442",
    name: "arch-subset-under-parent",
    description: "Inner `%ifarch` block lists every arch already guaranteed by an enclosing \
                  `%ifarch` — the inner test is always true.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Inner `%ifarch` block lists every arch already guaranteed by an enclosing `%ifarch` — the inner test is always true.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct ArchSubsetUnderParent {
    diagnostics: Vec<Diagnostic>,
    /// Stack of arch sets from enclosing `%ifarch` blocks. `None` slots
    /// mark `%if` blocks that aren't arch-flavoured but still nest.
    stack: Vec<Option<BTreeSet<String>>>,
}

impl ArchSubsetUnderParent {
    pub fn new() -> Self {
        Self::default()
    }

    fn maybe_emit(&mut self, inner: &BTreeSet<String>, anchor: Span) {
        for outer in self.stack.iter().rev().flatten() {
            if outer.is_subset(inner) {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "inner `%ifarch` list is a superset of an enclosing `%ifarch` list — \
                         the inner test always passes",
                        anchor,
                    )
                    .with_suggestion(Suggestion::new(
                        "unfold the inner `%ifarch` block; the parent already guarantees the arch \
                         set",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
                return;
            }
        }
    }
}

fn arch_list_of(branch_expr: &CondExpr<Span>, kind: CondKind) -> Option<BTreeSet<String>> {
    if !matches!(kind, CondKind::IfArch | CondKind::ElifArch) {
        return None;
    }
    match branch_expr {
        CondExpr::ArchList(list) => literal_archs(list),
        _ => None,
    }
}

fn walk_conditional<B, W>(
    rule: &mut ArchSubsetUnderParent,
    node: &Conditional<Span, B>,
    walk_body: W,
) where
    W: Fn(&mut ArchSubsetUnderParent, &[B]),
{
    for branch in &node.branches {
        let archs = arch_list_of(&branch.expr, branch.kind);
        if let Some(ref a) = archs {
            rule.maybe_emit(a, branch.data);
        }
        rule.stack.push(archs);
        walk_body(rule, &branch.body);
        rule.stack.pop();
    }
    if let Some(els) = node.otherwise.as_deref() {
        rule.stack.push(None);
        walk_body(rule, els);
        rule.stack.pop();
    }
}

impl<'ast> Visit<'ast> for ArchSubsetUnderParent {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for item in &spec.items {
            self.visit_item(item);
        }
    }

    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        walk_conditional(self, node, walk_top_body);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        walk_conditional(self, node, walk_preamble_body);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        walk_conditional(self, node, walk_files_body);
    }
}

impl Lint for ArchSubsetUnderParent {
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
        run_lint::<ArchSubsetUnderParent>(src)
    }

    #[test]
    fn flags_inner_superset_of_outer() {
        let src = "Name: x\n\
%ifarch x86_64 aarch64\n\
%ifarch x86_64 aarch64 ppc64le\n\
License: MIT\n\
%endif\n\
%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM442");
    }

    #[test]
    fn flags_inner_equal_to_outer() {
        // Inner equals outer — outer ⊆ inner is true with equality.
        let src = "Name: x\n\
%ifarch x86_64\n\
%ifarch x86_64\n\
License: MIT\n\
%endif\n\
%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn silent_when_inner_narrows() {
        // Inner is SUBSET of outer — inner adds restriction; not always true.
        let src = "Name: x\n\
%ifarch x86_64 aarch64\n\
%ifarch x86_64\n\
License: MIT\n\
%endif\n\
%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_independent_arch_lists() {
        let src = "Name: x\n\
%ifarch x86_64\n\
%ifarch aarch64\n\
License: MIT\n\
%endif\n\
%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_at_top_level() {
        // No enclosing arch guard.
        let src = "Name: x\n%ifarch x86_64 aarch64 ppc64le\nLicense: MIT\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn finds_subset_through_non_arch_layer() {
        // Intermediate `%if` doesn't break the arch reasoning.
        let src = "Name: x\n\
%ifarch x86_64 aarch64\n\
%if Z\n\
%ifarch x86_64 aarch64 ppc64le\n\
License: MIT\n\
%endif\n\
%endif\n\
%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }
}
