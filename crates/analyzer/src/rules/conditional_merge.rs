//! RPM076 `adjacent-mergeable-conditionals` — flag pairs of adjacent
//! `%if X` / `%endif` blocks (no items between them) that share the
//! same head expression and could be merged into a single block.
//!
//! Coverage:
//! - top-level `SpecItem` list
//! - `%package` preamble content list (`PreambleContent`)
//! - `%files` content list (`FilesContent`)
//!
//! "Adjacent" means consecutive entries in the item list. A blank
//! line between the two `%if` blocks shows up as a `Blank` item and
//! is allowed; any other item (preamble, comment, etc.) breaks
//! adjacency. We only flag the simplest case: two single-branch
//! `%if` blocks (no `%elif`, no `%else`) with literal expressions
//! that match per [`cond_expr_resolvably_eq`].
//!
//! Auto-fix is **Manual** for v1 — computing the exact byte ranges of
//! the `%endif` / `%if` keywords to splice out requires per-line
//! span information the AST doesn't yet expose.

use rpm_spec::ast::{
    CondKind, Conditional, FilesContent, PreambleContent, Section, Span, SpecFile, SpecItem,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::cond_expr_resolvably_eq;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM076",
    name: "adjacent-mergeable-conditionals",
    description: "Two adjacent `%if` blocks share the same condition; merge them into one block.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Two adjacent `%if` blocks share the same condition; merge them into one block.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct AdjacentMergeableConditionals {
    diagnostics: Vec<Diagnostic>,
}

impl AdjacentMergeableConditionals {
    pub fn new() -> Self {
        Self::default()
    }

    fn emit(&mut self, anchor: Span) {
        self.diagnostics.push(
            Diagnostic::new(
                &METADATA,
                Severity::Warn,
                "this `%if` repeats the condition of the previous adjacent block; merge them",
                anchor,
            )
            .with_suggestion(Suggestion::new(
                "delete the `%endif` / `%if EXPR` pair between the two bodies",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

/// `true` for an item that doesn't disrupt adjacency between two
/// `%if` blocks (currently only blank lines).
fn is_separator_top(item: &SpecItem<Span>) -> bool {
    matches!(item, SpecItem::Blank)
}
fn is_separator_preamble(item: &PreambleContent<Span>) -> bool {
    matches!(item, PreambleContent::Blank)
}
fn is_separator_files(item: &FilesContent<Span>) -> bool {
    matches!(item, FilesContent::Blank)
}

/// `true` when the `%if` block is "simple" — exactly one branch (no
/// `%elif`, no `%else`). Merging across multi-branch chains changes
/// the semantics (the second `%if` would re-evaluate after the first
/// `%else` runs), so we stay conservative.
fn is_simple_conditional<B>(c: &Conditional<Span, B>) -> bool {
    c.branches.len() == 1 && c.otherwise.is_none()
}

fn scan_pairs<I, F, G>(items: &[I], is_sep: F, get_conditional: G) -> Vec<Span>
where
    F: Fn(&I) -> bool,
    G: Fn(&I) -> Option<&Conditional<Span, I>>,
{
    let mut emits = Vec::new();
    let mut i = 0;
    while i < items.len() {
        let Some(c1) = get_conditional(&items[i]) else {
            i += 1;
            continue;
        };
        if !is_simple_conditional(c1) {
            i += 1;
            continue;
        }
        // Find the next item that isn't a separator (Blank).
        let mut j = i + 1;
        while j < items.len() && is_sep(&items[j]) {
            j += 1;
        }
        if j >= items.len() {
            break;
        }
        if let Some(c2) = get_conditional(&items[j])
            && is_simple_conditional(c2)
            && cond_expr_resolvably_eq(&c1.branches[0].expr, &c2.branches[0].expr)
        {
            emits.push(c2.data);
        }
        i = j;
    }
    emits
}

impl<'ast> Visit<'ast> for AdjacentMergeableConditionals {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        // Top-level pass.
        let emits = scan_pairs(&spec.items, is_separator_top, |it| match it {
            SpecItem::Conditional(c) => Some(c),
            _ => None,
        });
        for span in emits {
            self.emit(span);
        }
        // Subpackage preamble pass — recurse into each Section::Package.
        for item in &spec.items {
            if let SpecItem::Section(boxed) = item
                && let Section::Package { content, .. } = boxed.as_ref()
            {
                let emits = scan_pairs(content, is_separator_preamble, |it| match it {
                    PreambleContent::Conditional(c) => Some(c),
                    _ => None,
                });
                for span in emits {
                    self.emit(span);
                }
            }
        }
        // %files pass.
        for item in &spec.items {
            if let SpecItem::Section(boxed) = item
                && let Section::Files { content, .. } = boxed.as_ref()
            {
                let emits = scan_pairs(content, is_separator_files, |it| match it {
                    FilesContent::Conditional(c) => Some(c),
                    _ => None,
                });
                for span in emits {
                    self.emit(span);
                }
            }
        }
    }
}

impl Lint for AdjacentMergeableConditionals {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM084 if-not-x-after-if-x (Phase 7b) — arch/os forms only
// =====================================================================

pub static IF_NOT_X_AFTER_X_METADATA: LintMetadata = LintMetadata {
    id: "RPM084",
    name: "if-not-x-after-if-x",
    description: "Two adjacent `%ifarch X` / `%ifnarch X` blocks form a perfect `%else` — fold them.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Two adjacent `%if` blocks share the same condition; merge them into one block.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct IfNotXAfterIfX {
    diagnostics: Vec<Diagnostic>,
}

impl IfNotXAfterIfX {
    pub fn new() -> Self {
        Self::default()
    }

    fn emit(&mut self, anchor: Span) {
        self.diagnostics.push(
            Diagnostic::new(
                &IF_NOT_X_AFTER_X_METADATA,
                Severity::Warn,
                "adjacent `%ifarch X` and `%ifnarch X` (or `%ifos`/`%ifnos`) pair — \
                 fold the second block into an `%else`",
                anchor,
            )
            .with_suggestion(Suggestion::new(
                "merge into `%ifarch X ... %else ... %endif`",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

/// `true` when `a` and `b` are arch/os branch kinds whose semantics
/// are mutually exclusive — i.e. one is the negation of the other.
fn anti_arch_kinds(a: CondKind, b: CondKind) -> bool {
    matches!(
        (a, b),
        (CondKind::IfArch, CondKind::IfNArch)
            | (CondKind::IfNArch, CondKind::IfArch)
            | (CondKind::IfOs, CondKind::IfNOs)
            | (CondKind::IfNOs, CondKind::IfOs)
    )
}

fn scan_arch_anti_pairs<I, F, G>(items: &[I], is_sep: F, get_conditional: G) -> Vec<Span>
where
    F: Fn(&I) -> bool,
    G: Fn(&I) -> Option<&Conditional<Span, I>>,
{
    let mut emits = Vec::new();
    let mut i = 0;
    while i < items.len() {
        let Some(c1) = get_conditional(&items[i]) else {
            i += 1;
            continue;
        };
        if c1.branches.len() != 1 || c1.otherwise.is_some() {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < items.len() && is_sep(&items[j]) {
            j += 1;
        }
        if j >= items.len() {
            break;
        }
        if let Some(c2) = get_conditional(&items[j])
            && c2.branches.len() == 1
            && c2.otherwise.is_none()
            && anti_arch_kinds(c1.branches[0].kind, c2.branches[0].kind)
            && crate::rules::util::cond_expr_resolvably_eq(
                &c1.branches[0].expr,
                &c2.branches[0].expr,
            )
        {
            emits.push(c2.data);
        }
        i = j;
    }
    emits
}

impl<'ast> Visit<'ast> for IfNotXAfterIfX {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let emits = scan_arch_anti_pairs(&spec.items, is_separator_top, |it| match it {
            SpecItem::Conditional(c) => Some(c),
            _ => None,
        });
        for span in emits {
            self.emit(span);
        }
        for item in &spec.items {
            if let SpecItem::Section(boxed) = item
                && let Section::Package { content, .. } = boxed.as_ref()
            {
                let emits = scan_arch_anti_pairs(content, is_separator_preamble, |it| match it {
                    PreambleContent::Conditional(c) => Some(c),
                    _ => None,
                });
                for span in emits {
                    self.emit(span);
                }
            }
            if let SpecItem::Section(boxed) = item
                && let Section::Files { content, .. } = boxed.as_ref()
            {
                let emits = scan_arch_anti_pairs(content, is_separator_files, |it| match it {
                    FilesContent::Conditional(c) => Some(c),
                    _ => None,
                });
                for span in emits {
                    self.emit(span);
                }
            }
        }
    }
}

impl Lint for IfNotXAfterIfX {
    fn metadata(&self) -> &'static LintMetadata {
        &IF_NOT_X_AFTER_X_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM099 merge-elif-same-body (Phase 7c)
// =====================================================================

pub static MERGE_ELIF_METADATA: LintMetadata = LintMetadata {
    id: "RPM099",
    name: "merge-elif-same-body",
    description: "Two adjacent `%elif` branches share the same body — combine their conditions via `||`.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Two adjacent `%if` blocks share the same condition; merge them into one block.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct MergeElifSameBody {
    diagnostics: Vec<Diagnostic>,
    source: Option<std::sync::Arc<str>>,
}

impl MergeElifSameBody {
    pub fn new() -> Self {
        Self::default()
    }

    fn emit(&mut self, anchor: Span) {
        self.diagnostics.push(
            Diagnostic::new(
                &MERGE_ELIF_METADATA,
                Severity::Warn,
                "this `%elif` repeats the body of the previous branch — \
                 combine the two conditions with `||`",
                anchor,
            )
            .with_suggestion(Suggestion::new(
                "merge with the previous branch using `||`",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }

    fn check<B: HasBodySpan>(&mut self, node: &Conditional<Span, B>) {
        // Bump the `Arc` refcount so the loop body can call
        // `self.emit` without a borrow-checker conflict. Cheap —
        // no deep copy of the source string.
        let Some(source) = self.source.as_ref().map(std::sync::Arc::clone) else {
            return;
        };
        for i in 1..node.branches.len() {
            let prev = &node.branches[i - 1];
            let curr = &node.branches[i];
            // Only merge `%elif`/`%elif` pairs (or `%if` + first
            // `%elif`). Merging across kinds (e.g. `%if` + `%elifarch`)
            // is semantically risky.
            let mergeable_kinds = matches!(curr.kind, CondKind::Elif)
                && matches!(prev.kind, CondKind::If | CondKind::Elif);
            if !mergeable_kinds {
                continue;
            }
            if bodies_source_eq::<B>(&prev.body, &curr.body, &source) {
                self.emit(curr.data);
            }
        }
    }
}

/// Compute the byte range covered by a body's items and slice the
/// source. Returns `None` when the body has no item with a known
/// span.
pub(crate) fn body_text<'a, B: HasBodySpan>(body: &[B], source: &'a str) -> Option<&'a str> {
    let mut start: Option<usize> = None;
    let mut end: Option<usize> = None;
    for item in body {
        if let Some(sp) = item.body_span() {
            start.get_or_insert(sp.start_byte);
            end = Some(sp.end_byte);
        }
    }
    let (s, e) = (start?, end?);
    source.get(s..e)
}

pub(crate) fn bodies_source_eq<B: HasBodySpan>(a: &[B], b: &[B], source: &str) -> bool {
    let Some(t1) = body_text(a, source) else {
        return false;
    };
    let Some(t2) = body_text(b, source) else {
        return false;
    };
    // Normalise trailing whitespace per line so cosmetic differences
    // (trailing spaces) don't hide a true duplicate.
    let norm = |s: &str| -> String { s.lines().map(str::trim_end).collect::<Vec<_>>().join("\n") };
    norm(t1) == norm(t2)
}

/// Minimal span-extraction trait for body items. Mirrors the
/// per-context impls in `conditional_simplify.rs` — kept local to
/// avoid cross-module visibility churn for what is essentially three
/// match arms.
pub(crate) trait HasBodySpan {
    fn body_span(&self) -> Option<Span>;
}

impl HasBodySpan for SpecItem<Span> {
    fn body_span(&self) -> Option<Span> {
        match self {
            SpecItem::Preamble(p) => Some(p.data),
            SpecItem::Conditional(c) => Some(c.data),
            SpecItem::MacroDef(m) => Some(m.data),
            SpecItem::BuildCondition(b) => Some(b.data),
            SpecItem::Include(i) => Some(i.data),
            SpecItem::Comment(c) => Some(c.data),
            // `SpecItem` is `#[non_exhaustive]`; remaining variants
            // (Section, Statement, Blank, and any future addition)
            // do not have a body span this rule cares about.
            _ => None,
        }
    }
}

impl HasBodySpan for PreambleContent<Span> {
    fn body_span(&self) -> Option<Span> {
        match self {
            PreambleContent::Item(p) => Some(p.data),
            PreambleContent::Conditional(c) => Some(c.data),
            PreambleContent::Comment(c) => Some(c.data),
            // Blank + future `#[non_exhaustive]` variants.
            _ => None,
        }
    }
}

impl HasBodySpan for FilesContent<Span> {
    fn body_span(&self) -> Option<Span> {
        match self {
            FilesContent::Entry(e) => Some(e.data),
            FilesContent::Conditional(c) => Some(c.data),
            FilesContent::Comment(c) => Some(c.data),
            // Blank + future `#[non_exhaustive]` variants.
            _ => None,
        }
    }
}

impl<'ast> Visit<'ast> for MergeElifSameBody {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        self.check(node);
        crate::visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        self.check(node);
        crate::visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        self.check(node);
        crate::visit::walk_files_conditional(self, node);
    }
}

impl Lint for MergeElifSameBody {
    fn metadata(&self) -> &'static LintMetadata {
        &MERGE_ELIF_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_source(&mut self, source: std::sync::Arc<str>) {
        self.source = Some(source);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::test_support::run_lint;
    use crate::session::parse;

    fn run(src: &str) -> Vec<Diagnostic> {
        run_lint::<AdjacentMergeableConditionals>(src)
    }

    #[test]
    fn flags_adjacent_same_condition() {
        let src = "Name: x\n%if 1\nVersion: 1\n%endif\n%if 1\nRelease: 1\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM076");
    }

    #[test]
    fn flags_with_blank_line_between() {
        // Blank line doesn't disrupt adjacency.
        let src = "Name: x\n%if 1\nVersion: 1\n%endif\n\n%if 1\nRelease: 1\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn silent_for_distinct_conditions() {
        let src = "Name: x\n%if 1\nVersion: 1\n%endif\n%if 0\nRelease: 1\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_preamble_between() {
        let src = "Name: x\n%if 1\nVersion: 1\n%endif\nLicense: MIT\n%if 1\nRelease: 1\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_multi_branch_block() {
        // First block has %else — merging would change semantics.
        let src =
            "Name: x\n%if 1\nVersion: 1\n%else\nVersion: 2\n%endif\n%if 1\nRelease: 1\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_macro_condition() {
        // Conservative bail-out on macros — we can't claim the two
        // `%if 0%{?rhel}` blocks are equivalent without evaluating.
        let src = "Name: x\n%if 0%{?rhel}\nVersion: 1\n%endif\n%if 0%{?rhel}\nRelease: 1\n%endif\n";
        assert!(run(src).is_empty());
    }

    fn run_anti(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = IfNotXAfterIfX::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn rpm084_flags_ifarch_then_ifnarch() {
        let src = "Name: x\n%ifarch x86_64\nLicense: MIT\n%endif\n\
                   %ifnarch x86_64\nLicense: GPL\n%endif\n";
        let diags = run_anti(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM084");
    }

    #[test]
    fn rpm084_silent_for_same_kind() {
        let src = "Name: x\n%ifarch x86_64\nLicense: MIT\n%endif\n\
                   %ifarch x86_64\nLicense: GPL\n%endif\n";
        assert!(run_anti(src).is_empty());
    }

    #[test]
    fn rpm084_silent_for_distinct_arches() {
        let src = "Name: x\n%ifarch x86_64\nLicense: MIT\n%endif\n\
                   %ifnarch i386\nLicense: GPL\n%endif\n";
        assert!(run_anti(src).is_empty());
    }

    fn run_merge(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = MergeElifSameBody::new();
        lint.set_source(std::sync::Arc::from(src));
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn rpm099_flags_elif_with_same_body() {
        let src = "Name: x\n%if A\nLicense: MIT\n%elif B\nLicense: MIT\n%endif\n";
        let diags = run_merge(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM099");
    }

    #[test]
    fn rpm099_silent_for_different_bodies() {
        let src = "Name: x\n%if A\nLicense: MIT\n%elif B\nLicense: GPL\n%endif\n";
        assert!(run_merge(src).is_empty());
    }

    #[test]
    fn rpm099_flags_chain_of_three() {
        // Three consecutive same-body branches → two diagnostics
        // (the second and third).
        let src =
            "Name: x\n%if A\nLicense: MIT\n%elif B\nLicense: MIT\n%elif C\nLicense: MIT\n%endif\n";
        let diags = run_merge(src);
        assert_eq!(diags.len(), 2, "{diags:?}");
    }
}
