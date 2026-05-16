//! RPM119 `common-leaf-line-hoistable`.
//!
//! Walks every nested `%if`/`%elif`/`%else` block and reports lines
//! that appear on **every leaf path** of the tree. The canonical
//! example is a multi-arm distro switch whose every branch ends with
//! the same `BuildRequires: gcc-c++` — the line can be hoisted out
//! of the whole conditional.
//!
//! ## Difference from RPM097/098 (HoistCommonPrefix/Suffix)
//!
//! The Phase 7c hoist rules examine only the *immediate* branches of
//! a single `%if`-block (siblings). They do not recurse through inner
//! conditionals, so they miss the common-across-leaves pattern when
//! arms themselves contain nested splits. RPM119 covers that case:
//! it collects the set of lines present along every root-to-leaf
//! path and reports their intersection.
//!
//! ## When it stays silent
//!
//! - The conditional has no explicit `%else`. The implicit fall-through
//!   path contributes the empty set, so the intersection is empty.
//! - Any leaf path contains zero line-comparable items (e.g. only
//!   comments / blanks).
//! - The rule reports only on the **outermost** conditional reached
//!   during the visit pass. Inner sub-trees never emit their own
//!   diagnostic, avoiding duplicates when the same line surfaces at
//!   multiple nesting depths.

use std::collections::BTreeSet;

use rpm_spec::ast::{Conditional, FilesContent, PreambleContent, Span, SpecItem};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::conditional_merge::HasBodySpan;
use crate::visit::{self, Visit};

pub static LEAF_HOIST_METADATA: LintMetadata = LintMetadata {
    id: "RPM119",
    name: "common-leaf-line-hoistable",
    description: "A line appears on every root-to-leaf path of a nested `%if` tree — it can be \
         hoisted outside the conditional to remove redundant duplication.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Cap on the number of distinct lines we report per outermost
/// conditional. A pathological tree with a deeply shared preamble
/// could otherwise produce dozens of diagnostics anchored at the
/// same span.
const MAX_REPORTED_LINES: usize = 6;

#[derive(Debug, Default)]
pub struct CommonLeafLineHoistable {
    diagnostics: Vec<Diagnostic>,
    source: Option<String>,
    /// Number of `%if` blocks currently being walked. The rule only
    /// fires when `depth == 0` (outermost) — the recursive
    /// path-set computation handles the entire sub-tree, so emitting
    /// at inner levels would just duplicate the same finding.
    depth: usize,
}

impl CommonLeafLineHoistable {
    pub fn new() -> Self {
        Self::default()
    }

    fn check<B: BodyNode>(&mut self, node: &Conditional<Span, B>) {
        let Some(source) = self.source.clone() else {
            return;
        };
        let common = lines_in_all_paths::<B>(node, &source);
        if common.is_empty() {
            return;
        }
        let mut shown: Vec<&str> = common
            .iter()
            .take(MAX_REPORTED_LINES)
            .map(String::as_str)
            .collect();
        // Stable order for readability + test determinism.
        shown.sort();
        let preview = shown.join("` / `");
        let extra = common.len().saturating_sub(MAX_REPORTED_LINES);
        let suffix = if extra > 0 {
            format!(" (+{extra} more)")
        } else {
            String::new()
        };
        self.diagnostics.push(
            Diagnostic::new(
                &LEAF_HOIST_METADATA,
                Severity::Warn,
                format!(
                    "every branch of this `%if` tree contains `{preview}`{suffix}; \
                     consider hoisting outside the conditional"
                ),
                node.data,
            )
            .with_suggestion(Suggestion::new(
                "move the shared line(s) above `%if` (or below `%endif`) and \
                 delete them from each branch",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

impl<'ast> Visit<'ast> for CommonLeafLineHoistable {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        if self.depth == 0 {
            self.check(node);
        }
        self.depth += 1;
        visit::walk_top_conditional(self, node);
        self.depth -= 1;
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        if self.depth == 0 {
            self.check(node);
        }
        self.depth += 1;
        visit::walk_preamble_conditional(self, node);
        self.depth -= 1;
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        if self.depth == 0 {
            self.check(node);
        }
        self.depth += 1;
        visit::walk_files_conditional(self, node);
        self.depth -= 1;
    }
}

impl Lint for CommonLeafLineHoistable {
    fn metadata(&self) -> &'static LintMetadata {
        &LEAF_HOIST_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_source(&mut self, source: &str) {
        self.source = Some(source.to_owned());
    }
}

// =====================================================================
// Path-set computation
// =====================================================================

/// Extension over [`HasBodySpan`]: lets us recognise a body item as a
/// nested `%if` block and recurse into it.
pub(crate) trait BodyNode: HasBodySpan + Sized {
    fn as_conditional(&self) -> Option<&Conditional<Span, Self>>;
}

impl BodyNode for SpecItem<Span> {
    fn as_conditional(&self) -> Option<&Conditional<Span, Self>> {
        match self {
            SpecItem::Conditional(c) => Some(c),
            _ => None,
        }
    }
}

impl BodyNode for PreambleContent<Span> {
    fn as_conditional(&self) -> Option<&Conditional<Span, Self>> {
        match self {
            PreambleContent::Conditional(c) => Some(c),
            _ => None,
        }
    }
}

impl BodyNode for FilesContent<Span> {
    fn as_conditional(&self) -> Option<&Conditional<Span, Self>> {
        match self {
            FilesContent::Conditional(c) => Some(c),
            _ => None,
        }
    }
}

/// Set of lines that appear on every root-to-leaf path of the tree
/// rooted at `node`. Returns the empty set when the conditional has
/// no `%else` (the implicit fall-through path contributes nothing)
/// or when any branch yields an empty set.
fn lines_in_all_paths<B: BodyNode>(node: &Conditional<Span, B>, source: &str) -> BTreeSet<String> {
    if node.otherwise.is_none() {
        return BTreeSet::new();
    }
    let mut iter = node
        .branches
        .iter()
        .map(|b| lines_in_path::<B>(&b.body, source));
    let Some(mut acc) = iter.next() else {
        return BTreeSet::new();
    };
    for next in iter {
        acc = &acc & &next;
        if acc.is_empty() {
            return acc;
        }
    }
    if let Some(els) = &node.otherwise {
        acc = &acc & &lines_in_path::<B>(els, source);
    }
    acc
}

/// Collect the union of lines visible along this path — including
/// lines hoisted via recursion through nested conditionals.
fn lines_in_path<B: BodyNode>(body: &[B], source: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for item in body {
        if let Some(inner) = item.as_conditional() {
            // A line common to all leaves of the inner tree is a line
            // visible on every path through this body — pull it up.
            out.extend(lines_in_all_paths::<B>(inner, source));
            continue;
        }
        if let Some(span) = item.body_span() {
            if let Some(slice) = source.get(span.start_byte..span.end_byte) {
                let canon = canonicalise_line(slice);
                if !canon.is_empty() {
                    out.insert(canon);
                }
            }
        }
    }
    out
}

/// Trim trailing whitespace per line and collapse multi-line items
/// (`%define foo bar \` continuations) to a stable canonical form so
/// minor formatting drift between copies doesn't hide a duplicate.
fn canonicalise_line(slice: &str) -> String {
    slice
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = CommonLeafLineHoistable::new();
        lint.set_source(src);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn rpm119_flags_line_shared_across_two_arms() {
        let src = "\
Name: x
%if A
BuildRequires: gcc-c++
BuildRequires: clang
%else
BuildRequires: gcc-c++
BuildRequires: llvm
%endif
";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM119");
        assert!(diags[0].message.contains("gcc-c++"));
    }

    #[test]
    fn rpm119_flags_line_shared_across_nested_arms() {
        // Tower-of-ifs pattern from real-world specs: a shared
        // line buried in every leaf of a 2-level nest.
        let src = "\
Name: x
%if A
BuildRequires: gcc-c++
%else
%if B
BuildRequires: gcc-c++
%else
BuildRequires: gcc-c++
%endif
%endif
";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("gcc-c++"));
    }

    #[test]
    fn rpm119_silent_without_else() {
        // No `%else` → implicit fall-through path has no lines,
        // intersection is empty.
        let src = "\
Name: x
%if A
BuildRequires: gcc-c++
%endif
";
        assert!(run(src).is_empty());
    }

    #[test]
    fn rpm119_silent_when_one_leaf_differs() {
        let src = "\
Name: x
%if A
BuildRequires: gcc-c++
%else
%if B
BuildRequires: gcc-c++
%else
BuildRequires: gcc5-c++
%endif
%endif
";
        // gcc-c++ vs gcc5-c++ in the deepest leaf → no strict overlap.
        assert!(run(src).is_empty());
    }

    #[test]
    fn rpm119_only_outermost_emits() {
        // Two nesting levels share a line. We expect EXACTLY one
        // diagnostic (the outermost), not two.
        let src = "\
Name: x
%if A
%if B
BuildRequires: gcc-c++
%else
BuildRequires: gcc-c++
%endif
%else
BuildRequires: gcc-c++
%endif
";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn rpm119_silent_when_no_overlap() {
        let src = "\
Name: x
%if A
BuildRequires: clang
%else
BuildRequires: gcc
%endif
";
        assert!(run(src).is_empty());
    }
}
