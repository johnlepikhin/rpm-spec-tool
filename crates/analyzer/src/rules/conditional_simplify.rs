//! Phase 6 simplification lints over `%if`/`%elif`/`%else` blocks.
//!
//! - RPM072 `constant-condition` — `%if 1` / `%if 0` (and `true`/`false`)
//!   have a known outcome at parse time; one branch is always dead.
//! - RPM074 `identical-conditional-branches` — every branch has the
//!   same body bytes; the block is a no-op.
//! - RPM075 `redundant-nested-condition` — an inner `%if` repeats a
//!   parent `%if`'s expression and can never differ in value.
//!
//! Auto-fixes are currently **Manual** because computing the exact
//! byte ranges to splice (`%if` keyword, body, `%else`, `%endif`) is
//! delicate — the AST stores body items with their own spans but
//! not the surrounding keyword positions. Diagnostics still anchor at
//! the offending block so the human knows where to look.

use rpm_spec::ast::{
    CondExpr, Conditional, FilesContent, PreambleContent, Section, Span, SpecItem,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{
    cond_expr_resolvably_eq, is_constant_false_condition, is_constant_true_condition,
};
use crate::visit::{self, Visit};

// =====================================================================
// RPM072 constant-condition
// =====================================================================

pub static CONSTANT_CONDITION_METADATA: LintMetadata = LintMetadata {
    id: "RPM072",
    name: "constant-condition",
    description:
        "`%if 0` / `%if 1` has a fixed outcome; drop the block or simplify to the live branch.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct ConstantCondition {
    diagnostics: Vec<Diagnostic>,
}

impl ConstantCondition {
    pub fn new() -> Self {
        Self::default()
    }

    fn check_first(&mut self, expr: &CondExpr<Span>, anchor: Span) {
        let (verdict, hint) = if is_constant_true_condition(expr) {
            ("always true", "drop the `%if`/`%endif` wrapper and keep the body")
        } else if is_constant_false_condition(expr) {
            (
                "always false",
                "drop the `%if` body; keep the `%else` body if present",
            )
        } else {
            return;
        };
        self.diagnostics.push(
            Diagnostic::new(
                &CONSTANT_CONDITION_METADATA,
                Severity::Warn,
                format!("`%if` condition is {verdict}; one branch is dead code"),
                anchor,
            )
            .with_suggestion(Suggestion::new(hint, Vec::new(), Applicability::Manual)),
        );
    }
}

impl<'ast> Visit<'ast> for ConstantCondition {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        if let Some(first) = node.branches.first() {
            self.check_first(&first.expr, node.data);
        }
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(
        &mut self,
        node: &'ast Conditional<Span, PreambleContent<Span>>,
    ) {
        if let Some(first) = node.branches.first() {
            self.check_first(&first.expr, node.data);
        }
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(
        &mut self,
        node: &'ast Conditional<Span, FilesContent<Span>>,
    ) {
        if let Some(first) = node.branches.first() {
            self.check_first(&first.expr, node.data);
        }
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for ConstantCondition {
    fn metadata(&self) -> &'static LintMetadata {
        &CONSTANT_CONDITION_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM074 identical-conditional-branches
// =====================================================================

pub static IDENTICAL_BRANCHES_METADATA: LintMetadata = LintMetadata {
    id: "RPM074",
    name: "identical-conditional-branches",
    description: "Every branch of this conditional has the same body — the block is a no-op.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct IdenticalConditionalBranches {
    diagnostics: Vec<Diagnostic>,
    source: Option<String>,
}

impl IdenticalConditionalBranches {
    pub fn new() -> Self {
        Self::default()
    }

    fn emit(&mut self, anchor: Span) {
        self.diagnostics.push(
            Diagnostic::new(
                &IDENTICAL_BRANCHES_METADATA,
                Severity::Warn,
                "all branches of this conditional contain the same body",
                anchor,
            )
            .with_suggestion(Suggestion::new(
                "replace the whole block with one copy of the shared body",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

/// Smallest abstraction over the three body-item types we visit, so a
/// single comparator can slice source for each.
trait BranchItem {
    fn span(&self) -> Option<Span>;
}

// Most of `SpecItem` / `PreambleContent` / `FilesContent` / `Section`
// is `#[non_exhaustive]`. We use catch-all `_ => None` arms so future
// variants from the upstream parser fall through without halting
// compilation — at worst the body becomes "non-comparable" and the
// rule stays silent. Lower risk than blocking on an upstream bump.

impl BranchItem for SpecItem<Span> {
    fn span(&self) -> Option<Span> {
        match self {
            SpecItem::Preamble(p) => Some(p.data),
            SpecItem::Section(s) => section_span(s.as_ref()),
            SpecItem::Conditional(c) => Some(c.data),
            SpecItem::MacroDef(m) => Some(m.data),
            SpecItem::BuildCondition(b) => Some(b.data),
            SpecItem::Include(i) => Some(i.data),
            SpecItem::Comment(c) => Some(c.data),
            SpecItem::Statement(_) | SpecItem::Blank => None,
            _ => None,
        }
    }
}

impl BranchItem for PreambleContent<Span> {
    fn span(&self) -> Option<Span> {
        match self {
            PreambleContent::Item(p) => Some(p.data),
            PreambleContent::Conditional(c) => Some(c.data),
            PreambleContent::Comment(c) => Some(c.data),
            PreambleContent::Blank => None,
            _ => None,
        }
    }
}

impl BranchItem for FilesContent<Span> {
    fn span(&self) -> Option<Span> {
        match self {
            FilesContent::Entry(e) => Some(e.data),
            FilesContent::Conditional(c) => Some(c.data),
            FilesContent::Comment(c) => Some(c.data),
            FilesContent::Blank => None,
            _ => None,
        }
    }
}

fn section_span<T: Copy>(s: &Section<T>) -> Option<T> {
    match s {
        Section::Package { data, .. }
        | Section::BuildScript { data, .. }
        | Section::Files { data, .. }
        | Section::Verify { data, .. }
        | Section::Changelog { data, .. }
        | Section::SourceList { data, .. }
        | Section::PatchList { data, .. }
        | Section::Sepolicy { data, .. } => Some(*data),
        Section::Scriptlet(sc) => Some(sc.data),
        Section::Trigger(t) => Some(t.data),
        Section::FileTrigger(t) => Some(t.data),
        _ => None,
    }
}

/// Compute the byte range covered by a body's non-blank items. Returns
/// `None` if the body has no item with a known span.
fn body_range<B: BranchItem>(body: &[B]) -> Option<(usize, usize)> {
    let mut start: Option<usize> = None;
    let mut end: Option<usize> = None;
    for item in body {
        if let Some(sp) = item.span() {
            start.get_or_insert(sp.start_byte);
            end = Some(sp.end_byte);
        }
    }
    Some((start?, end?))
}

fn body_text<B: BranchItem>(body: &[B], source: &str) -> Option<String> {
    let (s, e) = body_range(body)?;
    let slice = source.get(s..e)?;
    // Normalise line-trailing whitespace so cosmetic differences
    // don't hide a true duplicate.
    Some(slice.lines().map(str::trim_end).collect::<Vec<_>>().join("\n"))
}

impl IdenticalConditionalBranches {
    fn check<B: BranchItem>(&mut self, node: &Conditional<Span, B>) {
        let Some(source) = self.source.as_deref() else { return };
        let total = node.branches.len() + usize::from(node.otherwise.is_some());
        if total < 2 {
            return;
        }
        let Some(first) = body_text(&node.branches[0].body, source) else { return };
        for branch in &node.branches[1..] {
            let Some(text) = body_text(&branch.body, source) else { return };
            if text != first {
                return;
            }
        }
        if let Some(other) = &node.otherwise {
            let Some(text) = body_text(other, source) else { return };
            if text != first {
                return;
            }
        }
        self.emit(node.data);
    }
}

impl<'ast> Visit<'ast> for IdenticalConditionalBranches {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        self.check(node);
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(
        &mut self,
        node: &'ast Conditional<Span, PreambleContent<Span>>,
    ) {
        self.check(node);
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(
        &mut self,
        node: &'ast Conditional<Span, FilesContent<Span>>,
    ) {
        self.check(node);
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for IdenticalConditionalBranches {
    fn metadata(&self) -> &'static LintMetadata {
        &IDENTICAL_BRANCHES_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_source(&mut self, source: &str) {
        self.source = Some(source.to_owned());
    }
}

// =====================================================================
// RPM075 redundant-nested-condition
// =====================================================================

pub static REDUNDANT_NESTED_METADATA: LintMetadata = LintMetadata {
    id: "RPM075",
    name: "redundant-nested-condition",
    description:
        "Inner `%if` repeats an enclosing `%if`'s condition; the inner test always passes.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct RedundantNestedCondition {
    diagnostics: Vec<Diagnostic>,
    /// Stack of currently-open `%if` head expressions. Owned clones
    /// keep the visit pass lifetime-free; `CondExpr` is small (a Text
    /// plus a discriminant).
    stack: Vec<CondExpr<Span>>,
}

impl RedundantNestedCondition {
    pub fn new() -> Self {
        Self::default()
    }

    fn enter(&mut self, expr: &CondExpr<Span>, anchor: Span) {
        if self.stack.iter().any(|p| cond_expr_resolvably_eq(p, expr)) {
            self.diagnostics.push(
                Diagnostic::new(
                    &REDUNDANT_NESTED_METADATA,
                    Severity::Warn,
                    "inner `%if` repeats an enclosing condition and is always satisfied",
                    anchor,
                )
                .with_suggestion(Suggestion::new(
                    "drop the inner `%if`/`%endif` wrapper",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
        self.stack.push(expr.clone());
    }

    fn leave(&mut self) {
        self.stack.pop();
    }
}

impl<'ast> Visit<'ast> for RedundantNestedCondition {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        let pushed = if let Some(first) = node.branches.first() {
            self.enter(&first.expr, node.data);
            true
        } else {
            false
        };
        visit::walk_top_conditional(self, node);
        if pushed {
            self.leave();
        }
    }
    fn visit_preamble_conditional(
        &mut self,
        node: &'ast Conditional<Span, PreambleContent<Span>>,
    ) {
        let pushed = if let Some(first) = node.branches.first() {
            self.enter(&first.expr, node.data);
            true
        } else {
            false
        };
        visit::walk_preamble_conditional(self, node);
        if pushed {
            self.leave();
        }
    }
    fn visit_files_conditional(
        &mut self,
        node: &'ast Conditional<Span, FilesContent<Span>>,
    ) {
        let pushed = if let Some(first) = node.branches.first() {
            self.enter(&first.expr, node.data);
            true
        } else {
            false
        };
        visit::walk_files_conditional(self, node);
        if pushed {
            self.leave();
        }
    }
}

impl Lint for RedundantNestedCondition {
    fn metadata(&self) -> &'static LintMetadata {
        &REDUNDANT_NESTED_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        self.stack.clear();
        std::mem::take(&mut self.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run<L: Lint>(src: &str, mut lint: L) -> Vec<Diagnostic> {
        let outcome = parse(src);
        lint.set_source(src);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    // ---- RPM072 ----

    #[test]
    fn rpm072_flags_if_1() {
        let src = "Name: x\n%if 1\nVersion: 1\n%endif\n";
        let diags = run(src, ConstantCondition::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM072");
        assert!(diags[0].message.contains("always true"));
    }

    #[test]
    fn rpm072_flags_if_0() {
        let src = "Name: x\n%if 0\nVersion: 1\n%endif\n";
        let diags = run(src, ConstantCondition::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("always false"));
    }

    #[test]
    fn rpm072_silent_for_macro_condition() {
        let src = "Name: x\n%if 0%{?rhel}\nVersion: 1\n%endif\n";
        assert!(run(src, ConstantCondition::new()).is_empty());
    }

    // ---- RPM074 ----

    #[test]
    fn rpm074_flags_identical_branches() {
        let src = "Name: x\n%if 0\nVersion: 1\n%else\nVersion: 1\n%endif\n";
        let diags = run(src, IdenticalConditionalBranches::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM074");
    }

    #[test]
    fn rpm074_silent_for_different_branches() {
        let src = "Name: x\n%if 0\nVersion: 1\n%else\nVersion: 2\n%endif\n";
        assert!(run(src, IdenticalConditionalBranches::new()).is_empty());
    }

    #[test]
    fn rpm074_silent_for_single_branch() {
        let src = "Name: x\n%if 0\nVersion: 1\n%endif\n";
        assert!(run(src, IdenticalConditionalBranches::new()).is_empty());
    }

    // ---- RPM075 ----

    #[test]
    fn rpm075_flags_nested_same_condition() {
        let src = "Name: x\n%if 1\n%if 1\nVersion: 1\n%endif\n%endif\n";
        let diags = run(src, RedundantNestedCondition::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM075");
    }

    #[test]
    fn rpm075_silent_for_distinct_nested_conditions() {
        let src = "Name: x\n%if 0\n%if 1\nVersion: 1\n%endif\n%endif\n";
        assert!(run(src, RedundantNestedCondition::new()).is_empty());
    }

    #[test]
    fn rpm075_silent_for_nested_macros() {
        // Both conditions are macros; conservative bail-out applies.
        let src = "Name: x\n%if 0%{?rhel}\n%if 0%{?rhel}\nVersion: 1\n%endif\n%endif\n";
        assert!(run(src, RedundantNestedCondition::new()).is_empty());
    }
}
